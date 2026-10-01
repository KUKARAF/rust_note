//! `GET /api/pipeline` — aggregate pipeline items across `pipeline/` notes.

use axum::extract::{Query, State};
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use rust_note_core::frontmatter::Frontmatter;
use rust_note_core::leads::{Ball, Kind, PipelineItem};

use crate::auth::session::RequireAuth;
use crate::error::{AppError, AppResult};
use crate::notes::acl;
use crate::notes::fs_store::{is_valid_note_id, path_to_note_id};
use crate::state::AppState;

/// Note-id prefix every pipeline item lives under.
const PIPELINE_PREFIX: &str = "pipeline/";

pub fn router() -> Router<AppState> {
    Router::new().route("/api/pipeline", get(list_pipeline))
}

/// A [`PipelineItem`] plus the request-time derived `overdue` flag, flattened
/// so the wire shape is one flat object. `overdue` is kept here (not on the
/// core type) because it depends on "now".
#[derive(Debug, Serialize)]
pub(crate) struct PipelineEntry {
    #[serde(flatten)]
    item: PipelineItem,
    /// The ball is in play (`!= none`), a deadline exists, and it has passed.
    overdue: bool,
}

#[derive(Debug, Deserialize)]
pub(crate) struct PipelineQuery {
    /// `lead` | `application`; omitted means all kinds.
    #[serde(default)]
    pub(crate) kind: Option<String>,
    /// Include items that are closed (terminal stage or `ball == none`).
    /// Defaults to false — the board hides closed items.
    #[serde(default)]
    pub(crate) include_closed: bool,
}

pub(crate) async fn list_pipeline(
    State(state): State<AppState>,
    RequireAuth(user_id): RequireAuth,
    Query(query): Query<PipelineQuery>,
) -> AppResult<Json<Vec<PipelineEntry>>> {
    // A supplied `kind` must be a valid vocabulary value; an unknown one is a
    // client error rather than a silent "match nothing".
    let kind_filter =
        match query.kind.as_deref().map(str::trim) {
            Some(k) if !k.is_empty() => Some(Kind::parse(k).ok_or_else(|| {
                AppError::BadRequest("kind must be 'lead' or 'application'".into())
            })?),
            _ => None,
        };

    let paths = state.notes_repo.list_notes().map_err(AppError::Internal)?;
    let now = OffsetDateTime::now_utc();

    let mut entries = Vec::new();
    for rel_path in paths {
        let note_id = path_to_note_id(&rel_path);
        if !is_valid_note_id(&note_id) || !note_id.starts_with(PIPELINE_PREFIX) {
            continue;
        }

        // Same lazy-adopt + ACL gate as the todos/notes list, so externally
        // created pipeline notes are included and other users' notes filtered.
        acl::adopt_if_orphaned(&state.db, &note_id, &user_id)
            .await
            .map_err(AppError::Internal)?;
        if !acl::can_read(&state.db, &note_id, &user_id)
            .await
            .map_err(AppError::Internal)?
        {
            continue;
        }

        // Prefer a live collab room's in-memory text over disk (a note open in
        // the editor is only flushed after a debounce), mirroring the todos
        // route.
        let content = match state.rooms.get(&note_id) {
            Some(room) => room.snapshot_text(),
            None => state
                .notes_repo
                .read_file(&rel_path)
                .map_err(AppError::Internal)?
                .unwrap_or_default(),
        };

        let fm = Frontmatter::parse(&content);
        let Some(item) = PipelineItem::from_frontmatter(&note_id, &fm) else {
            continue;
        };

        if kind_filter.is_some_and(|k| k != item.kind) {
            continue;
        }
        if !query.include_closed && item.is_closed() {
            continue;
        }

        let overdue = item.ball != Ball::None && item.expected_at.is_some_and(|e| now > e);
        entries.push(PipelineEntry { item, overdue });
    }

    Ok(Json(entries))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::oidc::OidcClient;
    use crate::config::Config;
    use crate::notes::fs_store::note_id_to_path;
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
        sqlx::query(
            "INSERT INTO users (id, email, display_name, created_at) \
             VALUES ('alice', 'alice@example.com', 'Alice', '2026-01-01T00:00:00Z')",
        )
        .execute(&db)
        .await
        .unwrap();
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

    async fn seed(state: &AppState, note_id: &str, content: &str) {
        let rel = note_id_to_path(note_id);
        acl::ensure_note_registered(&state.db, note_id, "alice")
            .await
            .unwrap();
        state
            .notes_repo
            .write_and_commit(&rel, content, "alice", "alice@example.com", "seed")
            .await
            .unwrap();
    }

    async fn run(state: &AppState, kind: Option<&str>, include_closed: bool) -> Vec<PipelineEntry> {
        list_pipeline(
            State(state.clone()),
            RequireAuth("alice".to_string()),
            Query(PipelineQuery {
                kind: kind.map(str::to_string),
                include_closed,
            }),
        )
        .await
        .unwrap()
        .0
    }

    #[tokio::test]
    async fn aggregates_only_pipeline_notes_and_hides_closed_by_default() {
        let (state, _n, _d) = test_state().await;
        // Open application, ball ours.
        seed(
            &state,
            "pipeline/affirm-swe",
            "---\nkind: application\ncompany: Affirm\nstage: interview\nball: ours\n---\n# Affirm\n",
        )
        .await;
        // Closed lead (terminal stage) — hidden unless include_closed.
        seed(
            &state,
            "pipeline/acme-gig",
            "---\nkind: lead\ncompany: Acme\nstage: won\nball: none\n---\n# Acme\n",
        )
        .await;
        // A non-pipeline note must never appear.
        seed(&state, "diary/2026-10-01", "- [ ] not a pipeline item\n").await;

        let open = run(&state, None, false).await;
        assert_eq!(open.len(), 1, "only the one open pipeline item");
        assert_eq!(open[0].item.id, "pipeline/affirm-swe");

        let all = run(&state, None, true).await;
        assert_eq!(all.len(), 2, "include_closed surfaces the closed item too");
    }

    #[tokio::test]
    async fn kind_filter_selects_one_tracker() {
        let (state, _n, _d) = test_state().await;
        seed(
            &state,
            "pipeline/affirm-swe",
            "---\nkind: application\ncompany: Affirm\nball: ours\n---\n",
        )
        .await;
        seed(
            &state,
            "pipeline/acme-gig",
            "---\nkind: lead\ncompany: Acme\nball: ours\n---\n",
        )
        .await;

        let leads = run(&state, Some("lead"), false).await;
        assert_eq!(leads.len(), 1);
        assert_eq!(leads[0].item.company, "Acme");

        let apps = run(&state, Some("application"), false).await;
        assert_eq!(apps.len(), 1);
        assert_eq!(apps[0].item.company, "Affirm");
    }

    #[tokio::test]
    async fn invalid_kind_is_bad_request() {
        let (state, _n, _d) = test_state().await;
        let err = list_pipeline(
            State(state.clone()),
            RequireAuth("alice".to_string()),
            Query(PipelineQuery {
                kind: Some("wishlist".to_string()),
                include_closed: false,
            }),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, AppError::BadRequest(_)));
    }

    #[tokio::test]
    async fn overdue_is_derived_from_ball_and_expected_at() {
        let (state, _n, _d) = test_state().await;
        // ball in play + a deadline in the past → overdue.
        seed(
            &state,
            "pipeline/past-due",
            "---\nkind: application\ncompany: Past\nball: theirs\nexpected_at: 2000-01-01T00:00:00Z\n---\n",
        )
        .await;
        // ball in play + a deadline far in the future → not overdue.
        seed(
            &state,
            "pipeline/future",
            "---\nkind: application\ncompany: Future\nball: ours\nexpected_at: 2999-01-01T00:00:00Z\n---\n",
        )
        .await;
        // No deadline → never overdue even with the ball in play.
        seed(
            &state,
            "pipeline/no-date",
            "---\nkind: lead\ncompany: NoDate\nball: ours\n---\n",
        )
        .await;

        let entries = run(&state, None, false).await;
        let by_id = |id: &str| {
            entries
                .iter()
                .find(|e| e.item.id == id)
                .map(|e| e.overdue)
                .unwrap()
        };
        assert!(by_id("pipeline/past-due"));
        assert!(!by_id("pipeline/future"));
        assert!(!by_id("pipeline/no-date"));
    }

    #[tokio::test]
    async fn response_flattens_item_fields_with_overdue() {
        let (state, _n, _d) = test_state().await;
        seed(
            &state,
            "pipeline/affirm-swe",
            "---\nkind: application\ncompany: Affirm\nball: theirs\nexpected_at: 2000-01-01T00:00:00Z\n---\n",
        )
        .await;

        let entries = run(&state, None, false).await;
        let json = serde_json::to_value(&entries[0]).unwrap();
        // Flattened: item fields sit at the top level alongside `overdue`.
        assert_eq!(json["id"], "pipeline/affirm-swe");
        assert_eq!(json["kind"], "application");
        assert_eq!(json["company"], "Affirm");
        assert_eq!(json["ball"], "theirs");
        assert_eq!(json["overdue"], true);
    }
}
