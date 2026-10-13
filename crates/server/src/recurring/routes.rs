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
    default_number_emojis, fetch_has_unread, http_client, is_safe_recurring_url, load_settings,
    local_done, read_note_text, today_daily_note_id, RecurringKind, RecurringSettings,
    RecurringTodo, RecurringTodoStatus, DEFS_NOTE_ID,
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
    number_emojis: i64,
}

async fn list_recurring(
    State(state): State<AppState>,
    RequireAuth(user_id): RequireAuth,
) -> AppResult<Json<RecurringListResponse>> {
    let settings = load_settings(&state);
    let defs = settings.todos;
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

    Ok(Json(RecurringListResponse {
        todos,
        number_emojis: settings.number_emojis,
    }))
}

// ---- PUT /api/recurring ---------------------------------------------------

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct PutRecurringRequest {
    todos: Vec<RecurringTodo>,
    #[serde(default = "default_number_emojis")]
    number_emojis: i64,
}

async fn put_recurring(
    State(state): State<AppState>,
    RequireAuth(user_id): RequireAuth,
    WithRejection(Json(body), _): WithRejection<Json<PutRecurringRequest>, AppError>,
) -> AppResult<Json<PutRecurringRequest>> {
    if body.number_emojis < 1 {
        return Err(AppError::BadRequest(
            "number_emojis must be at least 1".to_string(),
        ));
    }

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

    let settings = RecurringSettings {
        number_emojis: body.number_emojis,
        todos,
    };
    let json = serde_json::to_string_pretty(&settings)
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

    Ok(Json(PutRecurringRequest {
        todos: settings.todos,
        number_emojis: settings.number_emojis,
    }))
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

/// Returns up to `number_emojis` emojis (`text/plain`, concatenated with no
/// separator), one per pending recurring todo in priority order, or `✅`
/// when nothing is pending.
///
/// Meant to be polled by an external status widget, so it is intentionally
/// unauthenticated. Recurring definitions are a global note and "today" is
/// resolved in the default timezone.
async fn ascii_todo(
    State(state): State<AppState>,
) -> impl IntoResponse {
    let emoji = compute_ascii_emoji(&state).await;
    ([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], emoji)
}

async fn compute_ascii_emoji(state: &AppState) -> String {
    let settings = load_settings(state);
    let defs = settings.todos;
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

    // Collect every pending todo's (order, emoji); `sort_by_key` is stable,
    // so ties keep their original array position (first seen wins), same as
    // before this collected more than just the single best one.
    let mut pending: Vec<(i64, String)> = Vec::new();
    for def in &defs {
        let is_pending = match def.kind {
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
        if is_pending {
            pending.push((def.order, def.emoji.clone()));
        }
    }

    if pending.is_empty() {
        return ALL_DONE_EMOJI.to_string();
    }
    pending.sort_by_key(|(order, _)| *order);
    let take = settings.number_emojis.max(1) as usize;
    pending
        .into_iter()
        .take(take)
        .map(|(_, emoji)| emoji)
        .collect()
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

    fn local_todo(key: &str, emoji: &str, order: i64) -> RecurringTodo {
        RecurringTodo {
            key: key.to_string(),
            label: key.to_string(),
            emoji: emoji.to_string(),
            order,
            kind: RecurringKind::Local,
            url: None,
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
            number_emojis: 1,
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
        assert!(load_settings(&state).todos.is_empty());
    }

    #[tokio::test]
    async fn put_accepts_a_normal_url() {
        let (state, _notes_dir, _db_dir) = test_state().await;

        let body = PutRecurringRequest {
            todos: vec![foreign_todo("mail", "https://mail.example.com/status")],
            number_emojis: 1,
        };
        let resp = put_recurring(
            State(state.clone()),
            RequireAuth("alice".to_string()),
            WithRejection(Json(body), std::marker::PhantomData),
        )
        .await
        .unwrap();
        assert_eq!(resp.0.todos.len(), 1);
        assert_eq!(load_settings(&state).todos.len(), 1);
    }

    #[tokio::test]
    async fn ascii_todo_returns_up_to_number_emojis_pending_items_in_order() {
        let (state, _notes_dir, _db_dir) = test_state().await;

        let body = PutRecurringRequest {
            todos: vec![
                local_todo("stretches", "🧘", 2),
                local_todo("mail", "🔔", 0),
                local_todo("standup", "📅", 1),
            ],
            number_emojis: 2,
        };
        put_recurring(
            State(state.clone()),
            RequireAuth("alice".to_string()),
            WithRejection(Json(body), std::marker::PhantomData),
        )
        .await
        .unwrap();

        // None of the three are marked done, so all are pending; only the top
        // 2 by `order` (mail=0, standup=1) should show, in that order.
        assert_eq!(compute_ascii_emoji(&state).await, "🔔📅");
    }

    #[tokio::test]
    async fn ascii_todo_caps_at_the_number_of_actually_pending_items() {
        let (state, _notes_dir, _db_dir) = test_state().await;

        let body = PutRecurringRequest {
            todos: vec![local_todo("mail", "🔔", 0)],
            number_emojis: 5,
        };
        put_recurring(
            State(state.clone()),
            RequireAuth("alice".to_string()),
            WithRejection(Json(body), std::marker::PhantomData),
        )
        .await
        .unwrap();

        assert_eq!(compute_ascii_emoji(&state).await, "🔔");
    }

    #[tokio::test]
    async fn ascii_todo_all_done_still_shows_checkmark_regardless_of_number_emojis() {
        let (state, _notes_dir, _db_dir) = test_state().await;

        let body = PutRecurringRequest {
            todos: vec![],
            number_emojis: 3,
        };
        put_recurring(
            State(state.clone()),
            RequireAuth("alice".to_string()),
            WithRejection(Json(body), std::marker::PhantomData),
        )
        .await
        .unwrap();

        assert_eq!(compute_ascii_emoji(&state).await, ALL_DONE_EMOJI);
    }

    #[tokio::test]
    async fn put_rejects_number_emojis_below_one() {
        let (state, _notes_dir, _db_dir) = test_state().await;

        let body = PutRecurringRequest {
            todos: vec![],
            number_emojis: 0,
        };
        let err = put_recurring(
            State(state.clone()),
            RequireAuth("alice".to_string()),
            WithRejection(Json(body), std::marker::PhantomData),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, AppError::BadRequest(_)));
    }
}
