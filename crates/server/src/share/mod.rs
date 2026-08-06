//! Share-link management (owner-facing REST) + the public guest resolve
//! endpoint.
//!
//! Four routes, mounted by [`router`] into the top-level API router
//! ([`crate::routes::build`]):
//!
//! * `POST   /api/shares`          — create a share link for a note (owner-only).
//! * `GET    /api/shares?note_id=` — list a note's share links (owner-only).
//! * `DELETE /api/shares/{token}`  — revoke a share link (owner-only).
//! * `GET    /api/shared/{token}`  — **public**, no auth: resolve a token to
//!   the note a guest is allowed to open.
//!
//! Share management is deliberately **not** nested under
//! `/api/notes/{*note_id}/...`: note ids are captured by a `{*note_id}`
//! wildcard elsewhere (`/api/notes/{*path}`), and axum 0.8 can't reliably
//! route a literal subpath (`/shares`) *after* a wildcard segment in the
//! same tree. Passing `note_id` in the request body (create) / query string
//! (list) instead sidesteps that entirely.
//!
//! The guest collaborative-editing WebSocket (`GET /ws/shared/{token}`) is a
//! sibling of this module but lives in [`crate::collab::ws`] alongside the
//! authenticated `/ws/notes/*` handler it shares its sync loop with.

pub mod store;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use axum_extra::extract::WithRejection;
use serde::{Deserialize, Serialize};

use crate::auth::session::RequireAuth;
use crate::error::{AppError, AppResult};
use crate::notes::acl;
use crate::notes::fs_store::{is_valid_note_id, note_id_to_path};
use crate::notes::metadata::build_meta;
use crate::state::AppState;
use store::{ShareLink, SharePermission, ShareTtl};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/shares", post(create_share).get(list_shares))
        .route("/api/shares/{token}", axum::routing::delete(revoke_share))
        .route("/api/shared/{token}", get(resolve_shared))
}

// ---- shared response shaping ----------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum ShareStatus {
    Active,
    Expired,
}

#[derive(Debug, Serialize)]
struct ShareLinkResponse {
    token: String,
    note_id: String,
    permission: SharePermission,
    created_at: String,
    /// `null` means the link never expires.
    expires_at: Option<String>,
    status: ShareStatus,
    /// Ready-to-share URL for the frontend guest page:
    /// `{base_url}/shared/{token}`.
    url: String,
}

/// `None` if `expires_at` is the internal "never expires" sentinel — see
/// `store` module docs.
fn public_expires_at(expires_at: &str) -> Option<String> {
    if expires_at == store::NEVER_EXPIRES_SENTINEL {
        None
    } else {
        Some(expires_at.to_string())
    }
}

fn to_response(state: &AppState, link: ShareLink) -> ShareLinkResponse {
    let status = if link.is_expired() {
        ShareStatus::Expired
    } else {
        ShareStatus::Active
    };
    let expires_at = public_expires_at(&link.expires_at);
    let base_url = state.config.base_url.trim_end_matches('/');
    let url = format!("{base_url}/shared/{}", link.token);

    ShareLinkResponse {
        token: link.token,
        note_id: link.note_id,
        permission: link.permission,
        created_at: link.created_at,
        expires_at,
        status,
        url,
    }
}

/// Verify `user_id` owns `note_id`, distinguishing "doesn't exist" (404)
/// from "exists but isn't yours" (403) — shared by all three owner-gated
/// share-management routes.
async fn require_owner(state: &AppState, note_id: &str, user_id: &str) -> AppResult<()> {
    if !is_valid_note_id(note_id) {
        return Err(AppError::BadRequest("invalid note id".to_string()));
    }
    if !acl::note_exists(&state.db, note_id)
        .await
        .map_err(AppError::Internal)?
    {
        return Err(AppError::NotFound);
    }
    if !acl::is_owner(&state.db, note_id, user_id)
        .await
        .map_err(AppError::Internal)?
    {
        return Err(AppError::Forbidden);
    }
    Ok(())
}

// ---- POST /api/shares ------------------------------------------------------

#[derive(Debug, Deserialize)]
struct CreateShareRequest {
    note_id: String,
    permission: SharePermission,
    /// `"24h"`, `"14d"`, or `"never"`.
    expires_in: String,
}

async fn create_share(
    State(state): State<AppState>,
    RequireAuth(user_id): RequireAuth,
    WithRejection(Json(body), _): WithRejection<Json<CreateShareRequest>, AppError>,
) -> AppResult<Json<ShareLinkResponse>> {
    require_owner(&state, &body.note_id, &user_id).await?;

    let ttl = ShareTtl::parse(&body.expires_in).ok_or_else(|| {
        AppError::BadRequest("expires_in must be one of \"24h\", \"14d\", or \"never\"".to_string())
    })?;

    let link = store::create(&state.db, &body.note_id, &user_id, body.permission, ttl)
        .await
        .map_err(AppError::Internal)?;

    Ok(Json(to_response(&state, link)))
}

// ---- GET /api/shares?note_id= ----------------------------------------------

#[derive(Debug, Deserialize)]
struct ListSharesQuery {
    note_id: String,
}

async fn list_shares(
    State(state): State<AppState>,
    RequireAuth(user_id): RequireAuth,
    Query(q): Query<ListSharesQuery>,
) -> AppResult<Json<Vec<ShareLinkResponse>>> {
    require_owner(&state, &q.note_id, &user_id).await?;

    let links = store::list_for_note(&state.db, &q.note_id, &user_id)
        .await
        .map_err(AppError::Internal)?;

    Ok(Json(
        links.into_iter().map(|l| to_response(&state, l)).collect(),
    ))
}

// ---- DELETE /api/shares/{token} --------------------------------------------

async fn revoke_share(
    State(state): State<AppState>,
    RequireAuth(user_id): RequireAuth,
    Path(token): Path<String>,
) -> AppResult<StatusCode> {
    let existing = store::find_any(&state.db, &token)
        .await
        .map_err(AppError::Internal)?
        .ok_or(AppError::NotFound)?;

    if existing.owner_id != user_id {
        return Err(AppError::Forbidden);
    }

    store::revoke(&state.db, &token, &user_id)
        .await
        .map_err(AppError::Internal)?;

    Ok(StatusCode::NO_CONTENT)
}

// ---- GET /api/shared/{token} (public) --------------------------------------

#[derive(Debug, Serialize)]
struct SharedNoteResponse {
    note_id: String,
    title: String,
    permission: SharePermission,
    owner_display_name: String,
    content: String,
    /// `null` means the link never expires.
    expires_at: Option<String>,
}

/// Look up a user's display name, falling back to the id — same fallback as
/// `collab::ws::display_name_for`.
async fn display_name_for(state: &AppState, user_id: &str) -> String {
    let row: Option<(Option<String>,)> =
        sqlx::query_as("SELECT display_name FROM users WHERE id = ?")
            .bind(user_id)
            .fetch_optional(&state.db)
            .await
            .ok()
            .flatten();
    row.and_then(|(name,)| name)
        .unwrap_or_else(|| user_id.to_string())
}

/// Public: resolve a share token to what the guest page needs to render —
/// no login, no ACL check beyond "this token is valid and unexpired". A
/// guest with a valid token is authorized to read that one note regardless
/// of the normal owner/`note_access` ACL (that authorization *is* the share
/// link).
///
/// An invalid or expired token both return a plain 404 — see
/// `store::resolve`'s docs for why the two aren't distinguished.
async fn resolve_shared(
    State(state): State<AppState>,
    Path(token): Path<String>,
) -> AppResult<Json<SharedNoteResponse>> {
    let link = store::resolve(&state.db, &token)
        .await
        .map_err(AppError::Internal)?
        .ok_or(AppError::NotFound)?;

    let rel_path = note_id_to_path(&link.note_id);
    let content = state
        .notes_repo
        .read_file(&rel_path)
        .map_err(AppError::Internal)?
        .unwrap_or_default();

    let meta = build_meta(&state, &link.note_id, &content)
        .await
        .map_err(AppError::Internal)?;
    let owner_display_name = display_name_for(&state, &link.owner_id).await;

    Ok(Json(SharedNoteResponse {
        note_id: link.note_id,
        title: meta.title,
        permission: link.permission,
        owner_display_name,
        content,
        expires_at: public_expires_at(&link.expires_at),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collab::test_support::test_state;
    use std::marker::PhantomData;

    async fn create_note(state: &AppState, id: &str, owner: &str, content: &str) {
        acl::ensure_note_registered(&state.db, id, owner)
            .await
            .unwrap();
        state
            .notes_repo
            .write_and_commit(&note_id_to_path(id), content, owner, "o@e", "seed")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn owner_can_create_list_and_revoke_a_share() {
        let (state, _notes_dir, _db_dir) = test_state().await;
        create_note(&state, "owned", "admin", "# Owned\nhello").await;

        let created = create_share(
            State(state.clone()),
            RequireAuth("admin".to_string()),
            WithRejection(
                Json(CreateShareRequest {
                    note_id: "owned".to_string(),
                    permission: SharePermission::Edit,
                    expires_in: "24h".to_string(),
                }),
                PhantomData,
            ),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(created.note_id, "owned");
        assert_eq!(created.status, ShareStatus::Active);
        assert!(created.url.ends_with(&format!("/shared/{}", created.token)));
        assert!(created.expires_at.is_some());

        let listed = list_shares(
            State(state.clone()),
            RequireAuth("admin".to_string()),
            Query(ListSharesQuery {
                note_id: "owned".to_string(),
            }),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].token, created.token);

        let status = revoke_share(
            State(state.clone()),
            RequireAuth("admin".to_string()),
            Path(created.token.clone()),
        )
        .await
        .unwrap();
        assert_eq!(status, StatusCode::NO_CONTENT);

        let listed_after = list_shares(
            State(state.clone()),
            RequireAuth("admin".to_string()),
            Query(ListSharesQuery {
                note_id: "owned".to_string(),
            }),
        )
        .await
        .unwrap()
        .0;
        assert!(listed_after.is_empty());
    }

    #[tokio::test]
    async fn non_owner_is_forbidden_from_creating_listing_or_revoking() {
        let (state, _notes_dir, _db_dir) = test_state().await;
        // Owned by "admin"; requests below come from "mallory".
        create_note(&state, "not-mallorys", "admin", "secret").await;

        let create_err = create_share(
            State(state.clone()),
            RequireAuth("mallory".to_string()),
            WithRejection(
                Json(CreateShareRequest {
                    note_id: "not-mallorys".to_string(),
                    permission: SharePermission::View,
                    expires_in: "24h".to_string(),
                }),
                PhantomData,
            ),
        )
        .await
        .unwrap_err();
        assert!(matches!(create_err, AppError::Forbidden));

        let list_err = list_shares(
            State(state.clone()),
            RequireAuth("mallory".to_string()),
            Query(ListSharesQuery {
                note_id: "not-mallorys".to_string(),
            }),
        )
        .await
        .unwrap_err();
        assert!(matches!(list_err, AppError::Forbidden));

        // A link the real owner created must not be revocable by mallory.
        let link = store::create(
            &state.db,
            "not-mallorys",
            "admin",
            SharePermission::View,
            ShareTtl::Never,
        )
        .await
        .unwrap();
        let revoke_err = revoke_share(
            State(state.clone()),
            RequireAuth("mallory".to_string()),
            Path(link.token.clone()),
        )
        .await
        .unwrap_err();
        assert!(matches!(revoke_err, AppError::Forbidden));
        // Still resolvable: mallory's failed attempt didn't revoke it.
        assert!(store::resolve(&state.db, &link.token)
            .await
            .unwrap()
            .is_some());
    }

    #[tokio::test]
    async fn create_share_for_nonexistent_note_is_not_found() {
        let (state, _notes_dir, _db_dir) = test_state().await;

        let err = create_share(
            State(state.clone()),
            RequireAuth("admin".to_string()),
            WithRejection(
                Json(CreateShareRequest {
                    note_id: "does-not-exist".to_string(),
                    permission: SharePermission::View,
                    expires_in: "24h".to_string(),
                }),
                PhantomData,
            ),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, AppError::NotFound));
    }

    #[tokio::test]
    async fn invalid_expires_in_is_bad_request() {
        let (state, _notes_dir, _db_dir) = test_state().await;
        create_note(&state, "owned2", "admin", "hi").await;

        let err = create_share(
            State(state.clone()),
            RequireAuth("admin".to_string()),
            WithRejection(
                Json(CreateShareRequest {
                    note_id: "owned2".to_string(),
                    permission: SharePermission::View,
                    expires_in: "3-fortnights".to_string(),
                }),
                PhantomData,
            ),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, AppError::BadRequest(_)));
    }

    #[tokio::test]
    async fn guest_resolve_returns_note_content_with_no_auth() {
        let (state, _notes_dir, _db_dir) = test_state().await;
        create_note(&state, "guest-note", "admin", "# Guest Note\nhello guest").await;

        let link = store::create(
            &state.db,
            "guest-note",
            "admin",
            SharePermission::Edit,
            ShareTtl::Days14,
        )
        .await
        .unwrap();

        let resolved = resolve_shared(State(state.clone()), Path(link.token.clone()))
            .await
            .unwrap()
            .0;
        assert_eq!(resolved.note_id, "guest-note");
        assert_eq!(resolved.title, "Guest Note");
        assert_eq!(resolved.content, "# Guest Note\nhello guest");
        assert_eq!(resolved.permission, SharePermission::Edit);
        assert!(resolved.expires_at.is_some());
    }

    #[tokio::test]
    async fn guest_resolve_of_expired_or_unknown_token_is_not_found() {
        let (state, _notes_dir, _db_dir) = test_state().await;
        create_note(&state, "expiring-note", "admin", "content").await;

        // Unknown token.
        let err = resolve_shared(State(state.clone()), Path("bogus-token".to_string()))
            .await
            .unwrap_err();
        assert!(matches!(err, AppError::NotFound));

        // Expired token (inserted directly, bypassing ShareTtl).
        sqlx::query(
            "INSERT INTO share_links (token, note_id, owner_id, permission, created_at, expires_at) \
             VALUES ('expired-guest-token', 'expiring-note', 'admin', 'view', \
             '2020-01-01T00:00:00Z', '2020-01-02T00:00:00Z')",
        )
        .execute(&state.db)
        .await
        .unwrap();

        let err = resolve_shared(
            State(state.clone()),
            Path("expired-guest-token".to_string()),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, AppError::NotFound));
    }

    #[tokio::test]
    async fn guest_resolve_never_expiring_link_has_null_expires_at() {
        let (state, _notes_dir, _db_dir) = test_state().await;
        create_note(&state, "forever", "admin", "content").await;

        let link = store::create(
            &state.db,
            "forever",
            "admin",
            SharePermission::View,
            ShareTtl::Never,
        )
        .await
        .unwrap();

        let resolved = resolve_shared(State(state.clone()), Path(link.token))
            .await
            .unwrap()
            .0;
        assert_eq!(resolved.expires_at, None);
    }
}
