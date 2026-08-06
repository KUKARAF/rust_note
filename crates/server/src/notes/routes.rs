//! Note CRUD REST routes.

use axum::extract::{Path, State};
use axum::routing::get;
use axum::{Json, Router};
use axum_extra::extract::WithRejection;
use serde::{Deserialize, Serialize};

use crate::auth::session::RequireAuth;
use crate::error::{AppError, AppResult};
use crate::notes::acl;
use crate::notes::fs_store::{
    is_slug_clean, is_valid_note_id, note_id_to_path, path_to_note_id, slugify_note_id,
};
use crate::notes::metadata::{build_list_meta, build_meta, NoteMeta};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/notes", get(list_notes).post(create_note))
        .route(
            "/api/notes/{*path}",
            get(get_note).put(put_note).delete(delete_note),
        )
}

// ---- GET /api/notes ---------------------------------------------------

pub(crate) async fn list_notes(
    State(state): State<AppState>,
    RequireAuth(user_id): RequireAuth,
) -> AppResult<Json<Vec<NoteMeta>>> {
    let paths = state.notes_repo.list_notes().map_err(AppError::Internal)?;

    let mut metas = Vec::new();
    for rel_path in paths {
        let note_id = path_to_note_id(&rel_path);
        let readable = acl::can_read(&state.db, &note_id, &user_id)
            .await
            .map_err(AppError::Internal)?;
        if !readable {
            continue;
        }

        let content = state
            .notes_repo
            .read_file(&rel_path)
            .map_err(AppError::Internal)?
            .unwrap_or_default();
        // Use the cheap DB-backed meta builder here: the list must not walk
        // each note's git history (dos-01/dos-02).
        let meta = build_list_meta(&state, &note_id, &content)
            .await
            .map_err(AppError::Internal)?;
        metas.push(meta);
    }

    Ok(Json(metas))
}

// ---- GET /api/notes/*path ----------------------------------------------

#[derive(Debug, Serialize)]
struct NoteResponse {
    meta: NoteMeta,
    content: String,
}

async fn get_note(
    State(state): State<AppState>,
    RequireAuth(user_id): RequireAuth,
    Path(path): Path<String>,
) -> AppResult<Json<NoteResponse>> {
    let note_id = path_to_note_id(&path);
    if !is_valid_note_id(&note_id) {
        return Err(AppError::BadRequest("invalid note id".to_string()));
    }
    let rel_path = note_id_to_path(&note_id);

    if !acl::note_exists(&state.db, &note_id)
        .await
        .map_err(AppError::Internal)?
    {
        return Err(AppError::NotFound);
    }

    let readable = acl::can_read(&state.db, &note_id, &user_id)
        .await
        .map_err(AppError::Internal)?;
    if !readable {
        return Err(AppError::Forbidden);
    }

    let content = state
        .notes_repo
        .read_file(&rel_path)
        .map_err(AppError::Internal)?
        .ok_or(AppError::NotFound)?;

    let meta = build_meta(&state, &note_id, &content)
        .await
        .map_err(AppError::Internal)?;

    Ok(Json(NoteResponse { meta, content }))
}

// ---- PUT /api/notes/*path -----------------------------------------------

#[derive(Debug, Deserialize)]
struct PutNoteRequest {
    content: String,
    /// Optimistic-concurrency guard: the `version` (latest commit sha) the
    /// client last saw for this note, normally the `version` field of the
    /// `NoteMeta` returned by the `GET`/`PUT`/`POST` that populated the
    /// editor. If present and it no longer matches the note's current
    /// version, someone else has written a newer version in the meantime
    /// and this write is rejected with 409 rather than silently clobbering
    /// it. Omitted (`None`) skips the check, e.g. for a brand-new note
    /// auto-vivified by this same PUT, or callers that don't care.
    #[serde(default)]
    expected_version: Option<String>,
}

async fn put_note(
    State(state): State<AppState>,
    RequireAuth(user_id): RequireAuth,
    Path(path): Path<String>,
    WithRejection(Json(body), _): WithRejection<Json<PutNoteRequest>, AppError>,
) -> AppResult<Json<NoteMeta>> {
    let note_id = path_to_note_id(&path);
    if !is_valid_note_id(&note_id) {
        return Err(AppError::BadRequest("invalid note id".to_string()));
    }
    let rel_path = note_id_to_path(&note_id);

    // Hold the per-note-id lock across the whole exists-check -> git
    // commit -> DB-registration sequence so a concurrent DELETE on the
    // same note id can't interleave its DB/git effects with this PUT's
    // (see `state::NoteLocks`).
    let _note_guard = state.note_locks.lock(&note_id).await;

    let db_exists = acl::note_exists(&state.db, &note_id)
        .await
        .map_err(AppError::Internal)?;

    // A `notes` DB row can outlive its on-disk file (e.g. the file was
    // removed out-of-band -- manual filesystem cleanup, a botched git
    // operation, a future bug elsewhere that forgets to call
    // `deregister_note`). GET already treats that state as "gone" (404).
    // If PUT only consulted the DB row here, it would silently resurrect
    // the file under the *stale* owner_id/note_access grants left over
    // from before the file disappeared, indistinguishable from an
    // ordinary edit of a note that was never gone. Detect that desync and
    // deregister the stale row first, so such a PUT is treated as a fresh
    // creation (current user becomes owner, no leftover grants) rather
    // than a silent, stale-ACL "edit".
    let file_exists = state
        .notes_repo
        .read_file(&rel_path)
        .map_err(AppError::Internal)?
        .is_some();
    if db_exists && !file_exists {
        acl::deregister_note(&state.db, &note_id)
            .await
            .map_err(AppError::Internal)?;
    }
    let exists = db_exists && file_exists;

    if exists {
        let writable = acl::can_write(&state.db, &note_id, &user_id)
            .await
            .map_err(AppError::Internal)?;
        if !writable {
            return Err(AppError::Forbidden);
        }

        // Optimistic-concurrency check: if the client told us which
        // version it based this edit on, make sure nobody else has
        // committed a newer version since then. Still holding
        // `_note_guard` here, so this check-then-write can't race another
        // PUT to the same note id. See
        // `silent-lost-update-no-conflict-detection`.
        if let Some(expected) = &body.expected_version {
            let history = state
                .notes_repo
                .history(&rel_path)
                .await
                .map_err(AppError::Internal)?;
            let current_version = history.first().map(|c| c.sha.clone()).unwrap_or_default();
            if &current_version != expected {
                return Err(AppError::Conflict(
                    "note has been modified since you last loaded it; reload and reapply your \
                     changes"
                        .to_string(),
                ));
            }
        }
    } else {
        // This PUT would auto-vivify a brand-new note. Only allow that when
        // the id is already in canonical slug form (lowercase, hyphenated,
        // alphanumeric segments) -- the same form the create (POST)
        // endpoint always produces via `slugify_note_id`. Otherwise a
        // client could create notes whose id could never have come from
        // POST /api/notes, bypassing the slug invariant the rest of the
        // app assumes (see note-id-normalization-inconsistency-put-vs-post).
        if !is_slug_clean(&note_id) {
            return Err(AppError::BadRequest(
                "note does not exist; new notes must be created via POST /api/notes \
                 (PUT only updates existing notes, or creates one whose id is \
                 already in canonical slug form)"
                    .to_string(),
            ));
        }
        acl::ensure_note_registered(&state.db, &note_id, &user_id)
            .await
            .map_err(AppError::Internal)?;
    }

    write_note(&state, &user_id, &note_id, &rel_path, &body.content).await
}

async fn write_note(
    state: &AppState,
    user_id: &str,
    note_id: &str,
    rel_path: &str,
    content: &str,
) -> AppResult<Json<NoteMeta>> {
    let (author_name, author_email) = crate::db_users::commit_author(&state.db, user_id)
        .await
        .map_err(AppError::Internal)?;
    let message = format!("update {rel_path}");

    state
        .notes_repo
        .write_and_commit(rel_path, content, &author_name, &author_email, &message)
        .await
        .map_err(AppError::Internal)?;

    // Keep the DB `updated_at` current so the (git-history-free) list endpoint
    // reports an accurate last-modified time.
    acl::touch_updated_at(&state.db, note_id)
        .await
        .map_err(AppError::Internal)?;

    let meta = build_meta(state, note_id, content)
        .await
        .map_err(AppError::Internal)?;
    Ok(Json(meta))
}

// ---- POST /api/notes -----------------------------------------------------

#[derive(Debug, Deserialize)]
struct CreateNoteRequest {
    id_or_title: String,
    content: Option<String>,
}

async fn create_note(
    State(state): State<AppState>,
    RequireAuth(user_id): RequireAuth,
    WithRejection(Json(body), _): WithRejection<Json<CreateNoteRequest>, AppError>,
) -> AppResult<Json<NoteMeta>> {
    let note_id = slugify_note_id(&body.id_or_title);
    if note_id.is_empty() {
        return Err(AppError::BadRequest(
            "id_or_title must contain at least one alphanumeric character".to_string(),
        ));
    }
    if !is_valid_note_id(&note_id) {
        return Err(AppError::BadRequest("invalid note id".to_string()));
    }
    let rel_path = note_id_to_path(&note_id);

    // See put_note: serialize the whole exists-check -> git write -> DB
    // registration sequence per note id against concurrent PUT/DELETE.
    let _note_guard = state.note_locks.lock(&note_id).await;

    let exists = acl::note_exists(&state.db, &note_id)
        .await
        .map_err(AppError::Internal)?;
    if exists {
        return Err(AppError::Conflict(format!("note {note_id} already exists")));
    }
    // Also guard against an orphaned file on disk with no `notes` row.
    let file_exists = state
        .notes_repo
        .read_file(&rel_path)
        .map_err(AppError::Internal)?
        .is_some();
    if file_exists {
        return Err(AppError::Conflict(format!("note {note_id} already exists")));
    }

    acl::ensure_note_registered(&state.db, &note_id, &user_id)
        .await
        .map_err(AppError::Internal)?;

    let content = body.content.unwrap_or_default();
    write_note(&state, &user_id, &note_id, &rel_path, &content).await
}

// ---- DELETE /api/notes/*path ---------------------------------------------

async fn delete_note(
    State(state): State<AppState>,
    RequireAuth(user_id): RequireAuth,
    Path(path): Path<String>,
) -> AppResult<()> {
    let note_id = path_to_note_id(&path);
    if !is_valid_note_id(&note_id) {
        return Err(AppError::BadRequest("invalid note id".to_string()));
    }
    let rel_path = note_id_to_path(&note_id);

    // See put_note: serialize the whole exists-check -> git delete -> DB
    // deregistration sequence per note id against concurrent PUT/POST.
    let _note_guard = state.note_locks.lock(&note_id).await;

    if !acl::note_exists(&state.db, &note_id)
        .await
        .map_err(AppError::Internal)?
    {
        return Err(AppError::NotFound);
    }

    let owner = acl::is_owner(&state.db, &note_id, &user_id)
        .await
        .map_err(AppError::Internal)?;
    if !owner {
        return Err(AppError::Forbidden);
    }

    let (author_name, author_email) = crate::db_users::commit_author(&state.db, &user_id)
        .await
        .map_err(AppError::Internal)?;
    let message = format!("delete {rel_path}");

    state
        .notes_repo
        .delete_and_commit(&rel_path, &author_name, &author_email, &message)
        .await
        .map_err(AppError::Internal)?;

    acl::deregister_note(&state.db, &note_id)
        .await
        .map_err(AppError::Internal)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::oidc::OidcClient;
    use crate::config::Config;
    use crate::notes::fs_store;
    use crate::notes::repo::NotesRepo;
    use axum_extra::extract::cookie::Key;
    use std::sync::Arc;

    async fn test_state() -> (AppState, tempfile::TempDir, tempfile::TempDir) {
        let notes_dir = tempfile::tempdir().unwrap();
        let db_dir = tempfile::tempdir().unwrap();
        let db_path = db_dir.path().join("test.db");

        let notes_repo = NotesRepo::open_or_init(notes_dir.path().to_str().unwrap()).unwrap();
        let db = crate::db::init_pool(db_path.to_str().unwrap())
            .await
            .unwrap();

        for (id, email) in [("alice", "alice@example.com"), ("bob", "bob@example.com")] {
            sqlx::query(
                "INSERT INTO users (id, email, display_name, created_at) VALUES (?, ?, ?, ?)",
            )
            .bind(id)
            .bind(email)
            .bind(id)
            .bind("2026-01-01T00:00:00Z")
            .execute(&db)
            .await
            .unwrap();
        }

        let state = AppState {
            db,
            notes_repo,
            config: Arc::new(Config::from_env()),
            oidc: None::<Arc<OidcClient>>,
            cookie_key: Key::generate(),
            note_locks: crate::state::NoteLocks::new(),
            rooms: crate::collab::room::RoomRegistry::new(),
        };

        (state, notes_dir, db_dir)
    }

    #[tokio::test]
    async fn create_read_and_list_note() {
        let (state, _notes_dir, _db_dir) = test_state().await;

        let meta = create_note(
            State(state.clone()),
            RequireAuth("alice".to_string()),
            WithRejection(
                Json(CreateNoteRequest {
                    id_or_title: "My First Note".to_string(),
                    content: Some("# My First Note\nhello".to_string()),
                }),
                std::marker::PhantomData,
            ),
        )
        .await
        .unwrap()
        .0;

        assert_eq!(meta.id, "my-first-note");
        assert_eq!(meta.title, "My First Note");
        assert_eq!(meta.owner_id, "alice");

        let read = get_note(
            State(state.clone()),
            RequireAuth("alice".to_string()),
            Path("my-first-note".to_string()),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(read.content, "# My First Note\nhello");

        let listed = list_notes(State(state.clone()), RequireAuth("alice".to_string()))
            .await
            .unwrap()
            .0;
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, "my-first-note");
    }

    #[tokio::test]
    async fn list_uses_cheap_db_meta_not_git_history() {
        // dos-01/dos-02 regression: the list endpoint must not walk git
        // history. `build_list_meta` therefore leaves `version` empty and
        // sources timestamps from the DB, whereas the single-note GET still
        // returns a real git sha as `version`. Assert that split so the list
        // can't silently regress back to per-note history walks.
        let (state, _notes_dir, _db_dir) = test_state().await;

        let _ = create_note(
            State(state.clone()),
            RequireAuth("alice".to_string()),
            WithRejection(
                Json(CreateNoteRequest {
                    id_or_title: "timed-note".to_string(),
                    content: Some("# Timed\n".to_string()),
                }),
                std::marker::PhantomData,
            ),
        )
        .await
        .unwrap();

        let listed = list_notes(State(state.clone()), RequireAuth("alice".to_string()))
            .await
            .unwrap()
            .0;
        assert_eq!(listed.len(), 1);
        let list_meta = &listed[0];
        assert_eq!(list_meta.id, "timed-note");
        assert!(
            list_meta.version.is_empty(),
            "list meta must not carry a git version (would imply a history walk); got {:?}",
            list_meta.version
        );
        assert!(
            !list_meta.updated_at.is_empty(),
            "list meta updated_at must come from the DB and be populated"
        );

        // The single-note GET, by contrast, still returns a real git version.
        let single = get_note(
            State(state.clone()),
            RequireAuth("alice".to_string()),
            Path("timed-note".to_string()),
        )
        .await
        .unwrap()
        .0;
        assert!(
            !single.meta.version.is_empty(),
            "single-note GET must still return a real git version token"
        );
    }

    #[tokio::test]
    async fn get_note_with_traversal_path_is_bad_request_not_internal() {
        let (state, _notes_dir, _db_dir) = test_state().await;

        let err = get_note(
            State(state.clone()),
            RequireAuth("alice".to_string()),
            Path("../../etc/passwd".to_string()),
        )
        .await
        .unwrap_err();
        assert!(
            matches!(err, AppError::BadRequest(_)),
            "expected BadRequest, got {err:?}"
        );
    }

    #[tokio::test]
    async fn get_note_with_null_byte_is_bad_request_not_internal() {
        let (state, _notes_dir, _db_dir) = test_state().await;

        let err = get_note(
            State(state.clone()),
            RequireAuth("alice".to_string()),
            Path("_test_sweep\0foo".to_string()),
        )
        .await
        .unwrap_err();
        assert!(
            matches!(err, AppError::BadRequest(_)),
            "expected BadRequest, got {err:?}"
        );
    }

    #[tokio::test]
    async fn create_note_with_oversized_id_is_bad_request_not_internal() {
        let (state, _notes_dir, _db_dir) = test_state().await;

        // A title that slugifies to a single, absurdly long segment must not
        // reach the filesystem (which would otherwise fail with an OS-level
        // ENAMETOOLONG surfaced as a raw 500).
        let huge_title = "a".repeat(50_000);

        let result = create_note(
            State(state.clone()),
            RequireAuth("alice".to_string()),
            WithRejection(
                Json(CreateNoteRequest {
                    id_or_title: huge_title,
                    content: Some("x".to_string()),
                }),
                std::marker::PhantomData,
            ),
        )
        .await;

        // Either the id gets truncated to a safe length and the create
        // succeeds, or it's rejected as bad input -- either way it must
        // never surface as an opaque Internal/500.
        match result {
            Ok(meta) => assert!(meta.0.id.len() <= fs_store::MAX_ID_SEGMENT_LEN),
            Err(err) => assert!(
                matches!(err, AppError::BadRequest(_)),
                "expected BadRequest, got {err:?}"
            ),
        }
    }

    #[tokio::test]
    async fn create_note_with_many_deeply_nested_segments_is_bad_request_not_internal() {
        let (state, _notes_dir, _db_dir) = test_state().await;

        // Many *individually short* segments (e.g. "a/a/a/.../a") don't trip
        // the single-segment length cap, but joined together can still
        // exceed a safe total path length and blow past PATH_MAX once
        // resolved against the repo root -- this must be rejected as bad
        // input up front rather than surfacing as an OS-level
        // ENAMETOOLONG/internal-error once it reaches `create_dir_all`.
        let deeply_nested_title = "a/".repeat(3000);

        let result = create_note(
            State(state.clone()),
            RequireAuth("alice".to_string()),
            WithRejection(
                Json(CreateNoteRequest {
                    id_or_title: deeply_nested_title,
                    content: Some("x".to_string()),
                }),
                std::marker::PhantomData,
            ),
        )
        .await;

        let err = result.unwrap_err();
        assert!(
            matches!(err, AppError::BadRequest(_)),
            "expected BadRequest, got {err:?}"
        );
    }

    #[tokio::test]
    async fn duplicate_create_conflicts() {
        let (state, _notes_dir, _db_dir) = test_state().await;

        let _ = create_note(
            State(state.clone()),
            RequireAuth("alice".to_string()),
            WithRejection(
                Json(CreateNoteRequest {
                    id_or_title: "dup".to_string(),
                    content: None,
                }),
                std::marker::PhantomData,
            ),
        )
        .await
        .unwrap();

        let err = create_note(
            State(state.clone()),
            RequireAuth("alice".to_string()),
            WithRejection(
                Json(CreateNoteRequest {
                    id_or_title: "dup".to_string(),
                    content: None,
                }),
                std::marker::PhantomData,
            ),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, AppError::Conflict(_)));
    }

    #[tokio::test]
    async fn concurrent_create_of_same_id_only_one_wins() {
        // Regression test for create-delete-create-loop-registration-race:
        // two concurrent POST /api/notes with the same id_or_title must not
        // both succeed. The note-id-striped lock in `create_note` should
        // serialize the exists-check -> register -> write sequence so the
        // second racer observes the first's registration and gets a 409,
        // rather than both silently overwriting each other with 200s.
        let (state, _notes_dir, _db_dir) = test_state().await;

        let make_req = |content: &str| CreateNoteRequest {
            id_or_title: "dup-race".to_string(),
            content: Some(content.to_string()),
        };

        let state_a = state.clone();
        let state_b = state.clone();

        let task_a = tokio::spawn(async move {
            create_note(
                State(state_a),
                RequireAuth("alice".to_string()),
                WithRejection(
                    Json(make_req("AAAA content from request A")),
                    std::marker::PhantomData,
                ),
            )
            .await
        });
        let task_b = tokio::spawn(async move {
            create_note(
                State(state_b),
                RequireAuth("bob".to_string()),
                WithRejection(
                    Json(make_req("BBBB content from request B")),
                    std::marker::PhantomData,
                ),
            )
            .await
        });

        let (res_a, res_b) = tokio::join!(task_a, task_b);
        let res_a = res_a.unwrap();
        let res_b = res_b.unwrap();

        let ok_count = [res_a.is_ok(), res_b.is_ok()]
            .iter()
            .filter(|ok| **ok)
            .count();
        assert_eq!(
            ok_count,
            1,
            "exactly one of two concurrent creates for the same id must succeed; got a={:?} b={:?}",
            res_a.as_ref().map(|m| &m.0.owner_id),
            res_b.as_ref().map(|m| &m.0.owner_id),
        );
        for res in [&res_a, &res_b] {
            if let Err(err) = res {
                assert!(
                    matches!(err, AppError::Conflict(_)),
                    "loser should get Conflict, got {err:?}"
                );
            }
        }
    }

    #[tokio::test]
    async fn non_owner_forbidden_until_granted_then_respects_permission_level() {
        let (state, _notes_dir, _db_dir) = test_state().await;

        let _ = create_note(
            State(state.clone()),
            RequireAuth("alice".to_string()),
            WithRejection(
                Json(CreateNoteRequest {
                    id_or_title: "shared-note".to_string(),
                    content: Some("v1".to_string()),
                }),
                std::marker::PhantomData,
            ),
        )
        .await
        .unwrap();

        // bob has no access yet: shouldn't see it in the list, and can't
        // read or write it directly.
        let listed = list_notes(State(state.clone()), RequireAuth("bob".to_string()))
            .await
            .unwrap()
            .0;
        assert!(listed.is_empty());

        let err = get_note(
            State(state.clone()),
            RequireAuth("bob".to_string()),
            Path("shared-note".to_string()),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, AppError::Forbidden));

        let err = put_note(
            State(state.clone()),
            RequireAuth("bob".to_string()),
            Path("shared-note".to_string()),
            WithRejection(
                Json(PutNoteRequest {
                    content: "bob's edit".to_string(),
                    expected_version: None,
                }),
                std::marker::PhantomData,
            ),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, AppError::Forbidden));

        // Grant read-only access: bob can read but not write.
        acl::grant_access(&state.db, "shared-note", "bob", "read")
            .await
            .unwrap();

        let read = get_note(
            State(state.clone()),
            RequireAuth("bob".to_string()),
            Path("shared-note".to_string()),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(read.content, "v1");

        let err = put_note(
            State(state.clone()),
            RequireAuth("bob".to_string()),
            Path("shared-note".to_string()),
            WithRejection(
                Json(PutNoteRequest {
                    content: "bob's edit".to_string(),
                    expected_version: None,
                }),
                std::marker::PhantomData,
            ),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, AppError::Forbidden));

        // Upgrade to write access: bob can now write too.
        acl::grant_access(&state.db, "shared-note", "bob", "write")
            .await
            .unwrap();

        let meta = put_note(
            State(state.clone()),
            RequireAuth("bob".to_string()),
            Path("shared-note".to_string()),
            WithRejection(
                Json(PutNoteRequest {
                    content: "bob's edit".to_string(),
                    expected_version: None,
                }),
                std::marker::PhantomData,
            ),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(meta.id, "shared-note");

        let read = get_note(
            State(state.clone()),
            RequireAuth("alice".to_string()),
            Path("shared-note".to_string()),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(read.content, "bob's edit");
    }

    #[tokio::test]
    async fn only_owner_can_delete() {
        let (state, _notes_dir, _db_dir) = test_state().await;

        let _ = create_note(
            State(state.clone()),
            RequireAuth("alice".to_string()),
            WithRejection(
                Json(CreateNoteRequest {
                    id_or_title: "deletable".to_string(),
                    content: Some("bye".to_string()),
                }),
                std::marker::PhantomData,
            ),
        )
        .await
        .unwrap();

        acl::grant_access(&state.db, "deletable", "bob", "write")
            .await
            .unwrap();

        // bob has write access but is not the owner: delete must still be
        // forbidden for him.
        let err = delete_note(
            State(state.clone()),
            RequireAuth("bob".to_string()),
            Path("deletable".to_string()),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, AppError::Forbidden));

        delete_note(
            State(state.clone()),
            RequireAuth("alice".to_string()),
            Path("deletable".to_string()),
        )
        .await
        .unwrap();

        let err = get_note(
            State(state.clone()),
            RequireAuth("alice".to_string()),
            Path("deletable".to_string()),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, AppError::NotFound));

        let listed = list_notes(State(state.clone()), RequireAuth("alice".to_string()))
            .await
            .unwrap()
            .0;
        assert!(listed.is_empty());
    }

    #[tokio::test]
    async fn get_missing_note_is_not_found() {
        let (state, _notes_dir, _db_dir) = test_state().await;

        let err = get_note(
            State(state.clone()),
            RequireAuth("alice".to_string()),
            Path("nope".to_string()),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, AppError::NotFound));
    }

    #[tokio::test]
    async fn put_to_nonexistent_unslugified_id_is_rejected_not_auto_vivified() {
        let (state, _notes_dir, _db_dir) = test_state().await;

        // PUTting to a brand-new id that isn't already in canonical slug
        // form (mixed case, punctuation, leading underscore, spaces) must
        // be rejected rather than silently creating a note whose id could
        // never have come from POST /api/notes's slugify_note_id().
        let err = put_note(
            State(state.clone()),
            RequireAuth("alice".to_string()),
            Path("_test_sweep/Weird Id!".to_string()),
            WithRejection(
                Json(PutNoteRequest {
                    content: "hello".to_string(),
                    expected_version: None,
                }),
                std::marker::PhantomData,
            ),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, AppError::BadRequest(_)));

        // Nothing should have been created.
        let err = get_note(
            State(state.clone()),
            RequireAuth("alice".to_string()),
            Path("_test_sweep/Weird Id!".to_string()),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, AppError::NotFound));

        let listed = list_notes(State(state.clone()), RequireAuth("alice".to_string()))
            .await
            .unwrap()
            .0;
        assert!(listed.is_empty());
    }

    #[tokio::test]
    async fn put_cannot_create_case_variant_duplicate_of_existing_note() {
        let (state, _notes_dir, _db_dir) = test_state().await;

        // Create "test-sweep/casenote" the normal way, via POST, which
        // slugifies (lowercases) the id.
        let _ = create_note(
            State(state.clone()),
            RequireAuth("alice".to_string()),
            WithRejection(
                Json(CreateNoteRequest {
                    id_or_title: "_test_sweep/CaseNote".to_string(),
                    content: Some("lower version".to_string()),
                }),
                std::marker::PhantomData,
            ),
        )
        .await
        .unwrap();

        // A PUT to a mixed-case variant of the same slug must NOT be
        // allowed to auto-vivify a second, distinct note that differs from
        // the existing one only by case (regression test for
        // case-sensitive-duplicate-notes-same-slug-different-case): since
        // "test-sweep/CaseNote" isn't itself in canonical slug form, the
        // is_slug_clean guard in put_note must reject it.
        let err = put_note(
            State(state.clone()),
            RequireAuth("alice".to_string()),
            Path("test-sweep/CaseNote".to_string()),
            WithRejection(
                Json(PutNoteRequest {
                    content: "upper version".to_string(),
                    expected_version: None,
                }),
                std::marker::PhantomData,
            ),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, AppError::BadRequest(_)));

        // Only the original note exists; content wasn't clobbered and no
        // second note was created.
        let listed = list_notes(State(state.clone()), RequireAuth("alice".to_string()))
            .await
            .unwrap()
            .0;
        let sweep_ids: Vec<_> = listed
            .iter()
            .map(|m| m.id.as_str())
            .filter(|id| id.contains("sweep"))
            .collect();
        assert_eq!(sweep_ids, vec!["test-sweep/casenote"]);
    }

    #[tokio::test]
    async fn put_to_nonexistent_slug_clean_id_still_auto_vivifies() {
        let (state, _notes_dir, _db_dir) = test_state().await;

        // A PUT to an id that's already in canonical slug form (as if the
        // frontend is creating-by-saving with an id it computed itself)
        // should still work, preserving existing auto-vivify behavior.
        let meta = put_note(
            State(state.clone()),
            RequireAuth("alice".to_string()),
            Path("brand-new/clean-id".to_string()),
            WithRejection(
                Json(PutNoteRequest {
                    content: "hello".to_string(),
                    expected_version: None,
                }),
                std::marker::PhantomData,
            ),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(meta.id, "brand-new/clean-id");
        assert_eq!(meta.owner_id, "alice");
    }

    #[tokio::test]
    async fn put_to_db_row_whose_file_was_deleted_out_of_band_is_treated_as_new_note() {
        let (state, notes_dir, _db_dir) = test_state().await;

        // alice creates a note normally.
        let created = create_note(
            State(state.clone()),
            RequireAuth("alice".to_string()),
            WithRejection(
                Json(CreateNoteRequest {
                    id_or_title: "ghost-desync".to_string(),
                    content: Some("hello ghost".to_string()),
                }),
                std::marker::PhantomData,
            ),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(created.owner_id, "alice");

        // Simulate the file being removed out-of-band (manual filesystem
        // cleanup, a botched git operation, ...) while the `notes` DB row
        // is left behind -- i.e. deregister_note was never called.
        std::fs::remove_file(notes_dir.path().join("ghost-desync.md")).unwrap();

        // GET correctly reports the note as gone.
        let get_err = get_note(
            State(state.clone()),
            RequireAuth("alice".to_string()),
            Path("ghost-desync".to_string()),
        )
        .await
        .unwrap_err();
        assert!(matches!(get_err, AppError::NotFound));

        // A different user PUTs to the same id. Before the fix this would
        // silently succeed as an "edit" under alice's stale ownership
        // (can_write only consults the DB row), even though GET just said
        // the note doesn't exist. It must now be treated as a fresh
        // creation: bob becomes the owner, not a forbidden edit of alice's
        // old note.
        let resurrected = put_note(
            State(state.clone()),
            RequireAuth("bob".to_string()),
            Path("ghost-desync".to_string()),
            WithRejection(
                Json(PutNoteRequest {
                    content: "resurrected by bob".to_string(),
                    expected_version: None,
                }),
                std::marker::PhantomData,
            ),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(resurrected.id, "ghost-desync");
        assert_eq!(
            resurrected.owner_id, "bob",
            "resurrecting a desynced (DB row present, file missing) note must reset \
             ownership to the resurrecting user, not silently honor the stale prior owner"
        );

        let read = get_note(
            State(state.clone()),
            RequireAuth("bob".to_string()),
            Path("ghost-desync".to_string()),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(read.content, "resurrected by bob");
        assert_eq!(read.meta.owner_id, "bob");
    }

    #[tokio::test]
    async fn stale_expected_version_is_rejected_with_conflict_not_silently_overwritten() {
        // Regression test for `silent-lost-update-no-conflict-detection`:
        // two overlapping editing sessions (tab A / tab B) both start from
        // v1. B saves first; A's save is still based on the v1 it loaded
        // and must be rejected rather than silently destroying B's edit.
        let (state, _notes_dir, _db_dir) = test_state().await;

        let v1 = create_note(
            State(state.clone()),
            RequireAuth("alice".to_string()),
            WithRejection(
                Json(CreateNoteRequest {
                    id_or_title: "race-note".to_string(),
                    content: Some("v1-original".to_string()),
                }),
                std::marker::PhantomData,
            ),
        )
        .await
        .unwrap()
        .0;

        // Tab B: writer who has also seen v1, saves first based on it.
        let v2 = put_note(
            State(state.clone()),
            RequireAuth("alice".to_string()),
            Path("race-note".to_string()),
            WithRejection(
                Json(PutNoteRequest {
                    content: "v2-from-tabB".to_string(),
                    expected_version: Some(v1.version.clone()),
                }),
                std::marker::PhantomData,
            ),
        )
        .await
        .unwrap()
        .0;
        assert_ne!(
            v2.version, v1.version,
            "writing a new commit must produce a new version token"
        );

        // Tab A: still holds the stale v1 version token (it never saw B's
        // write) and now tries to save. This must be rejected with a
        // conflict rather than silently clobbering B's v2.
        let stale_err = put_note(
            State(state.clone()),
            RequireAuth("alice".to_string()),
            Path("race-note".to_string()),
            WithRejection(
                Json(PutNoteRequest {
                    content: "v3-from-tabA-based-on-stale-v1".to_string(),
                    expected_version: Some(v1.version.clone()),
                }),
                std::marker::PhantomData,
            ),
        )
        .await
        .unwrap_err();
        assert!(
            matches!(stale_err, AppError::Conflict(_)),
            "stale expected_version must be rejected as a conflict, got {stale_err:?}"
        );

        // B's write must still be intact -- not clobbered by A's rejected
        // request.
        let read = get_note(
            State(state.clone()),
            RequireAuth("alice".to_string()),
            Path("race-note".to_string()),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(read.content, "v2-from-tabB");
        assert_eq!(read.meta.version, v2.version);

        // A retry with the now-current version (as the UI would do after
        // reloading) succeeds normally.
        let v3 = put_note(
            State(state.clone()),
            RequireAuth("alice".to_string()),
            Path("race-note".to_string()),
            WithRejection(
                Json(PutNoteRequest {
                    content: "v3-rebased-on-v2".to_string(),
                    expected_version: Some(v2.version.clone()),
                }),
                std::marker::PhantomData,
            ),
        )
        .await
        .unwrap()
        .0;
        assert_ne!(v3.version, v2.version);

        // Omitting expected_version entirely (e.g. an older/other client)
        // still skips the check, preserving backward compatibility.
        let v4 = put_note(
            State(state.clone()),
            RequireAuth("alice".to_string()),
            Path("race-note".to_string()),
            WithRejection(
                Json(PutNoteRequest {
                    content: "v4-no-guard".to_string(),
                    expected_version: None,
                }),
                std::marker::PhantomData,
            ),
        )
        .await
        .unwrap()
        .0;
        assert_ne!(v4.version, v3.version);
    }
}
