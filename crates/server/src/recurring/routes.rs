//! HTTP handlers for `/api/recurring` (+ the ASCII-emoji status endpoint).

use axum::extract::{Path, State};
use axum::http::header;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use axum_extra::extract::WithRejection;
use serde::{Deserialize, Serialize};

use rust_note_core::frontmatter::Frontmatter;

use super::calendar::{calendar_row_active, fetch_calendars};
use super::{
    fetch_has_unread, http_client, is_safe_recurring_url, load_defs, local_done, read_note_text,
    today_daily_note_id, RecurringKind, RecurringTodo, RecurringTodoStatus, DEFS_NOTE_ID,
};
use crate::auth::session::RequireAuth;
use crate::collab::write::edit_note_through_room;
use crate::db_users::commit_author;
use crate::error::{AppError, AppResult};
use crate::notes::acl;
use crate::notes::fs_store::note_id_to_path;
use crate::settings::store::{load_or_bootstrap, settings_note_id};
use crate::state::AppState;
use crate::stats::{timezone_of, today_in_tz, DEFAULT_TZ};

/// Emoji returned by the ASCII-emoji status endpoint when nothing is pending.
const ALL_DONE_EMOJI: &str = "✅";

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/recurring", get(list_recurring).put(put_recurring))
        .route("/api/recurring/{key}/done", post(set_done))
        .route("/api/recurring_ascii_todo", get(ascii_todo))
}

// ---- shared helpers -------------------------------------------------------

/// The user's configured timezone, from their settings note (defaults to
/// [`DEFAULT_TZ`]). Bootstraps the settings note if it doesn't exist yet.
async fn settings_tz(state: &AppState, user_id: &str) -> AppResult<String> {
    let note_id = settings_note_id(user_id);
    let _guard = state.note_locks.lock(&note_id).await;
    load_or_bootstrap(state, user_id).await?;
    let rel = note_id_to_path(&note_id);
    let raw = state
        .notes_repo
        .read_file(&rel)
        .map_err(AppError::Internal)?
        .unwrap_or_default();
    Ok(timezone_of(&Frontmatter::parse(&raw)))
}

// ---- GET /api/recurring ---------------------------------------------------

#[derive(Debug, Serialize)]
pub(crate) struct RecurringListResponse {
    todos: Vec<RecurringTodoStatus>,
}

async fn list_recurring(
    State(state): State<AppState>,
    RequireAuth(user_id): RequireAuth,
) -> AppResult<Json<RecurringListResponse>> {
    let defs = load_defs(&state);
    let tz = settings_tz(&state, &user_id).await?;
    let daily_id = today_daily_note_id(&tz);
    let fm = Frontmatter::parse(&read_note_text(&state, &daily_id));

    // Reuse one hardened client for every foreign poll / ICS fetch this request
    // makes.
    let client = http_client().map_err(AppError::Internal)?;

    // Fetch + parse each distinct calendar url ONCE, then match every calendar
    // row's regex against the shared parsed result below.
    let calendar_urls = defs
        .iter()
        .filter(|d| d.kind == RecurringKind::Calendar)
        .filter_map(|d| d.url.clone());
    let calendars = fetch_calendars(&client, calendar_urls, today_in_tz(&tz)).await;

    let mut todos = Vec::with_capacity(defs.len());
    for def in defs {
        let done = match def.kind {
            RecurringKind::Local => local_done(&fm, &def.key),
            RecurringKind::Foreign => match def.url.as_deref() {
                // Satisfied when the source reports no unread; any fetch error
                // is reported as not-done (pending) for the modal.
                Some(url) => fetch_has_unread(&client, url)
                    .await
                    .map(|u| !u)
                    .unwrap_or(false),
                None => false,
            },
            // Satisfied unless a matching event occurs today (done = !active).
            RecurringKind::Calendar => {
                !calendar_row_active(&calendars, def.url.as_deref(), def.regex.as_deref())
            }
        };
        todos.push(RecurringTodoStatus {
            key: def.key,
            label: def.label,
            emoji: def.emoji,
            order: def.order,
            kind: def.kind,
            url: def.url,
            regex: def.regex,
            done,
        });
    }

    Ok(Json(RecurringListResponse { todos }))
}

// ---- PUT /api/recurring ---------------------------------------------------

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct PutRecurringRequest {
    todos: Vec<RecurringTodo>,
}

async fn put_recurring(
    State(state): State<AppState>,
    RequireAuth(user_id): RequireAuth,
    WithRejection(Json(body), _): WithRejection<Json<PutRecurringRequest>, AppError>,
) -> AppResult<Json<PutRecurringRequest>> {
    let todos: Vec<RecurringTodo> = body
        .todos
        .into_iter()
        .map(RecurringTodo::normalized)
        .collect();

    // `Foreign`/`Calendar` rows carry a server-fetched `url`: reject anything
    // that isn't a well-formed http(s) URL pointing at a public host, so a
    // stored definition can't be used to make the server probe loopback,
    // private, link-local, or cloud-metadata addresses (SSRF).
    for def in &todos {
        if let Some(url) = def.url.as_deref() {
            if !is_safe_recurring_url(url) {
                return Err(AppError::BadRequest(format!(
                    "invalid or unsafe url for recurring todo `{}`",
                    def.key
                )));
            }
        }
    }

    let json = serde_json::to_string_pretty(&todos)
        .map_err(|e| AppError::Internal(anyhow::anyhow!("serializing definitions failed: {e}")))?;

    let note_id = DEFS_NOTE_ID;
    let _guard = state.note_locks.lock(note_id).await;
    acl::ensure_note_registered(&state.db, note_id, &user_id)
        .await
        .map_err(AppError::Internal)?;

    let (author_name, author_email) = commit_author(&state.db, &user_id)
        .await
        .map_err(AppError::Internal)?;
    let rel = note_id_to_path(note_id);
    state
        .notes_repo
        .write_and_commit(
            &rel,
            &format!("{json}\n"),
            &author_name,
            &author_email,
            "update recurring todos",
        )
        .await
        .map_err(AppError::Internal)?;
    acl::touch_updated_at(&state.db, note_id)
        .await
        .map_err(AppError::Internal)?;

    Ok(Json(PutRecurringRequest { todos }))
}

// ---- POST /api/recurring/{key}/done ---------------------------------------

#[derive(Debug, Deserialize)]
pub(crate) struct SetDoneRequest {
    done: bool,
}

#[derive(Debug, Serialize)]
pub(crate) struct OkResponse {
    ok: bool,
}

async fn set_done(
    State(state): State<AppState>,
    RequireAuth(user_id): RequireAuth,
    Path(key): Path<String>,
    WithRejection(Json(body), _): WithRejection<Json<SetDoneRequest>, AppError>,
) -> AppResult<Json<OkResponse>> {
    let tz = settings_tz(&state, &user_id).await?;
    let note_id = today_daily_note_id(&tz);

    // Register (creating today's owned daily note when new) then authorize the
    // frontmatter write — a daily note owned by another user must not be
    // written through here.
    acl::ensure_note_registered(&state.db, &note_id, &user_id)
        .await
        .map_err(AppError::Internal)?;
    if !acl::can_write(&state.db, &note_id, &user_id)
        .await
        .map_err(AppError::Internal)?
    {
        return Err(AppError::Forbidden);
    }

    let value = if body.done { "true" } else { "false" };
    edit_note_through_room(&state, &note_id, &user_id, move |old| {
        let mut fm = Frontmatter::parse(old);
        fm.set(&key, value);
        Some(fm.render())
    })
    .await?;

    Ok(Json(OkResponse { ok: true }))
}

// ---- GET /api/recurring_ascii_todo -----------------------------------------

/// Returns a single emoji (`text/plain`) for the highest-priority pending
/// recurring todo, or `✅` when nothing is pending.
///
/// This used to be unauthenticated ("meant to be polled by an external status
/// widget"), but recurring definitions are a single *global* note shared by
/// every user, and computing the emoji makes the server issue live outbound
/// GETs to every configured `Foreign`/`Calendar` url. A public, unauthenticated
/// handler that does that is both an activity oracle (the emoji leaks whether
/// someone's task is pending) and — before the url validation added in
/// `put_recurring` — a blind SSRF primitive anyone on the internet could poke
/// at for free. Nothing in this codebase or the web app actually calls this
/// endpoint without a session, so there's no evidence it needs to stay public;
/// require auth like every other recurring route.
async fn ascii_todo(
    State(state): State<AppState>,
    RequireAuth(_user_id): RequireAuth,
) -> impl IntoResponse {
    let emoji = compute_ascii_emoji(&state).await;
    ([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], emoji)
}

async fn compute_ascii_emoji(state: &AppState) -> String {
    let defs = load_defs(state);
    // No per-user context on the public endpoint: resolve "today" in the
    // default timezone and read the (global) daily note directly.
    let daily_id = today_daily_note_id(DEFAULT_TZ);
    let fm = Frontmatter::parse(&read_note_text(state, &daily_id));

    let client = match http_client() {
        Ok(c) => c,
        Err(err) => {
            tracing::warn!(error = %err, "recurring ascii: HTTP client build failed");
            return ALL_DONE_EMOJI.to_string();
        }
    };

    // Fetch + parse each distinct calendar url once (public endpoint resolves
    // "today" in the default timezone).
    let calendar_urls = defs
        .iter()
        .filter(|d| d.kind == RecurringKind::Calendar)
        .filter_map(|d| d.url.clone());
    let calendars = fetch_calendars(&client, calendar_urls, today_in_tz(DEFAULT_TZ)).await;

    // Track the pending todo with the smallest order (0 = highest priority);
    // ties resolve to the earlier array position (first seen wins).
    let mut best: Option<(i64, String)> = None;
    for def in &defs {
        let pending = match def.kind {
            RecurringKind::Local => !local_done(&fm, &def.key),
            RecurringKind::Foreign => match def.url.as_deref() {
                Some(url) => match fetch_has_unread(&client, url).await {
                    Ok(has_unread) => has_unread,
                    Err(err) => {
                        // A source we can't reach is treated as NOT pending.
                        tracing::warn!(error = %err, url, "recurring ascii: foreign poll failed");
                        false
                    }
                },
                None => false,
            },
            // Pending exactly when a matching event occurs today.
            RecurringKind::Calendar => {
                calendar_row_active(&calendars, def.url.as_deref(), def.regex.as_deref())
            }
        };
        if pending && best.as_ref().is_none_or(|(o, _)| def.order < *o) {
            best = Some((def.order, def.emoji.clone()));
        }
    }

    best.map(|(_, emoji)| emoji)
        .unwrap_or_else(|| ALL_DONE_EMOJI.to_string())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum_extra::extract::cookie::Key;

    use super::*;
    use crate::auth::oidc::OidcClient;
    use crate::config::Config;
    use crate::notes::repo::NotesRepo;

    async fn test_state() -> (AppState, tempfile::TempDir, tempfile::TempDir) {
        let notes_dir = tempfile::tempdir().unwrap();
        let db_dir = tempfile::tempdir().unwrap();
        let db_path = db_dir.path().join("test.db");

        let notes_repo = NotesRepo::open_or_init(notes_dir.path().to_str().unwrap()).unwrap();
        let db = crate::db::init_pool(db_path.to_str().unwrap())
            .await
            .unwrap();

        sqlx::query("INSERT INTO users (id, email, display_name, created_at) VALUES (?, ?, ?, ?)")
            .bind("alice")
            .bind("alice@example.com")
            .bind("alice")
            .bind("2026-01-01T00:00:00Z")
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

    fn foreign_todo(key: &str, url: &str) -> RecurringTodo {
        RecurringTodo {
            key: key.to_string(),
            label: key.to_string(),
            emoji: "🔔".to_string(),
            order: 0,
            kind: RecurringKind::Foreign,
            url: Some(url.to_string()),
            regex: None,
        }
    }

    #[tokio::test]
    async fn put_rejects_an_ssrf_url() {
        let (state, _notes_dir, _db_dir) = test_state().await;

        let body = PutRecurringRequest {
            todos: vec![foreign_todo(
                "mail",
                "http://169.254.169.254/latest/meta-data",
            )],
        };
        let err = put_recurring(
            State(state.clone()),
            RequireAuth("alice".to_string()),
            WithRejection(Json(body), std::marker::PhantomData),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, AppError::BadRequest(_)));

        // Nothing should have been written.
        assert!(load_defs(&state).is_empty());
    }

    #[tokio::test]
    async fn put_accepts_a_normal_url() {
        let (state, _notes_dir, _db_dir) = test_state().await;

        let body = PutRecurringRequest {
            todos: vec![foreign_todo("mail", "https://mail.example.com/status")],
        };
        let resp = put_recurring(
            State(state.clone()),
            RequireAuth("alice".to_string()),
            WithRejection(Json(body), std::marker::PhantomData),
        )
        .await
        .unwrap();
        assert_eq!(resp.0.todos.len(), 1);
        assert_eq!(load_defs(&state).len(), 1);
    }
}
