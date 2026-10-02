//! `POST /api/settings/notify-test` — run the overdue-pipeline digest NOW for
//! the current user, ignoring the schedule and `notify_last_sent` (and without
//! updating it). Powers the Settings "Send test" button.

use axum::extract::State;
use axum::Json;
use serde::Serialize;
use time::OffsetDateTime;

use super::deliver_digest;
use crate::auth::session::RequireAuth;
use crate::error::{AppError, AppResult};
use crate::settings::store;
use crate::state::AppState;

/// Mirrors [`super::DigestOutcome`]. The token is never included.
#[derive(Debug, Serialize)]
pub(crate) struct NotifyTestResponse {
    sent: bool,
    count: usize,
    title: String,
    message: String,
}

pub(crate) async fn notify_test(
    State(state): State<AppState>,
    RequireAuth(user_id): RequireAuth,
) -> AppResult<Json<NotifyTestResponse>> {
    let settings = {
        let note_id = store::settings_note_id(&user_id);
        let _guard = state.note_locks.lock(&note_id).await;
        store::load_or_bootstrap(&state, &user_id).await?
    };

    if settings.notify_token.is_empty() {
        return Err(AppError::BadRequest(
            "configure the priority-notify API key first".to_string(),
        ));
    }

    // Run NOW, ignoring schedule + last_sent; `deliver_digest` never touches
    // `notify_last_sent`, so a test fire does not disturb the real cadence.
    let outcome = deliver_digest(&state, &user_id, &settings, OffsetDateTime::now_utc()).await?;

    Ok(Json(NotifyTestResponse {
        sent: outcome.sent,
        count: outcome.count,
        title: outcome.title,
        message: outcome.message,
    }))
}
