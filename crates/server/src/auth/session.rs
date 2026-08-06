//! Session management (backed by `tower-sessions` + `tower-sessions-sqlx-store`).
//!
//! We store only the authenticated user's `id` (the `users.id` primary key,
//! which is the OIDC `sub` claim) in the session. Anything else (email,
//! display name, ...) is looked up from the `users` table on demand — this
//! keeps the session payload tiny and avoids the classic bug where a stale
//! session carries around outdated profile info after the user updates it
//! upstream in Authentik.

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use tower_sessions::Session;

use crate::error::AppError;
use crate::state::AppState;

/// Session key under which the authenticated user's id is stored.
const USER_ID_KEY: &str = "user_id";

/// Record `user_id` as the authenticated identity for this session.
///
/// Also cycles the session id, which is standard practice to avoid session
/// fixation across a login boundary.
pub async fn login(session: &Session, user_id: &str) -> Result<(), AppError> {
    session
        .cycle_id()
        .await
        .map_err(|e| AppError::Internal(e.into()))?;
    session
        .insert(USER_ID_KEY, user_id)
        .await
        .map_err(|e| AppError::Internal(e.into()))?;
    Ok(())
}

/// Destroy the session entirely (clears data and removes it from the store).
pub async fn logout(session: &Session) -> Result<(), AppError> {
    session
        .flush()
        .await
        .map_err(|e| AppError::Internal(e.into()))?;
    Ok(())
}

/// Returns the authenticated user's id, if any, for the given session.
pub async fn current_user_id(session: &Session) -> Option<String> {
    session.get::<String>(USER_ID_KEY).await.ok().flatten()
}

/// Returns the effective authenticated user id for this request: the fixed
/// dev-mode user if [`crate::config::Config::dev_mode`] is enabled
/// (bypassing the session entirely), otherwise whatever's in the session.
///
/// This is the single place dev-mode's auth bypass is implemented - anything
/// that needs "who is the current user" (the `RequireAuth` extractor, the
/// `/auth/me` handler, ...) should go through this rather than checking
/// `config.dev_mode` itself, so the bypass behaves identically everywhere.
pub async fn effective_user_id(state: &AppState, session: &Session) -> Option<String> {
    if state.config.dev_mode {
        return Some(crate::config::DEV_MODE_USER_ID.to_string());
    }
    current_user_id(session).await
}

/// Axum extractor requiring an authenticated session.
///
/// Route handlers can take `RequireAuth` as an argument to get the current
/// user's id, or reject the request with `401 Unauthorized` if there isn't
/// one. This is the reusable primitive note CRUD / ACL routes build on.
///
/// In dev mode ([`crate::config::Config::dev_mode`]) this always succeeds
/// with the fixed user id `"admin"`, without inspecting the session at all.
pub struct RequireAuth(pub String);

impl FromRequestParts<AppState> for RequireAuth {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        if state.config.dev_mode {
            return Ok(RequireAuth(crate::config::DEV_MODE_USER_ID.to_string()));
        }

        let session = Session::from_request_parts(parts, state)
            .await
            .map_err(|_| AppError::Unauthorized)?;

        match current_user_id(&session).await {
            Some(user_id) => Ok(RequireAuth(user_id)),
            None => Err(AppError::Unauthorized),
        }
    }
}
