//! Per-user settings REST routes (`GET`/`PUT /api/settings`).

use axum::extract::State;
use axum::routing::{get, post};
use axum::{Json, Router};
use axum_extra::extract::WithRejection;
use serde::{Deserialize, Serialize};

use super::store::{
    self, is_valid_model_id, is_valid_notify_priority, is_valid_notify_scope, UserSettings,
    KNOWN_THEMES,
};
use crate::auth::session::RequireAuth;
use crate::error::{AppError, AppResult};
use crate::notes::{acl, fs_store::note_id_to_path};
use crate::notify;
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/settings", get(get_settings).put(put_settings))
        .route("/api/ai/models", get(get_ai_models))
        .route(
            "/api/settings/notify-test",
            post(notify::routes::notify_test),
        )
}

#[derive(Debug, Serialize)]
struct SettingsResponse {
    theme: String,
    openrouter_model: String,
    /// Whether an AI (LiteLLM) API key is stored. The key itself is NEVER
    /// returned — the client only needs to know if one is set.
    has_openrouter_key: bool,
    /// Base URL of the OpenAI-compatible AI endpoint (non-secret).
    ai_endpoint: String,

    // ---- priority-notify digest ----
    notify_enabled: bool,
    notify_endpoint: String,
    notify_priority: String,
    notify_scope: String,
    notify_schedule: String,
    /// Whether a priority-notify token is stored. The token itself is NEVER
    /// returned (mirrors `has_openrouter_key`).
    has_notify_token: bool,
}

impl SettingsResponse {
    fn from_settings(s: &UserSettings) -> Self {
        Self {
            theme: s.theme.clone(),
            openrouter_model: s.openrouter_model.clone(),
            has_openrouter_key: !s.openrouter_api_key.is_empty(),
            ai_endpoint: s.ai_endpoint.clone(),
            notify_enabled: s.notify_enabled,
            notify_endpoint: s.notify_endpoint.clone(),
            notify_priority: s.notify_priority.clone(),
            notify_scope: s.notify_scope.clone(),
            notify_schedule: s.notify_schedule.clone(),
            has_notify_token: !s.notify_token.is_empty(),
        }
    }
}

/// All fields optional so the client can update just one (e.g. only the theme,
/// or only the OpenRouter key) without echoing the others back.
#[derive(Debug, Default, Deserialize)]
struct PutSettingsRequest {
    theme: Option<String>,
    openrouter_model: Option<String>,
    openrouter_api_key: Option<String>,
    /// Base URL of the OpenAI-compatible AI endpoint; validated as an http(s)
    /// URL (non-secret).
    ai_endpoint: Option<String>,

    // ---- priority-notify digest ----
    notify_enabled: Option<bool>,
    notify_endpoint: Option<String>,
    notify_priority: Option<String>,
    notify_scope: Option<String>,
    notify_schedule: Option<String>,
    /// Write-only secret (like `openrouter_api_key`): accepted on PUT, never
    /// echoed back.
    notify_token: Option<String>,
}

async fn get_settings(
    State(state): State<AppState>,
    RequireAuth(user_id): RequireAuth,
) -> AppResult<Json<SettingsResponse>> {
    let note_id = store::settings_note_id(&user_id);
    let _guard = state.note_locks.lock(&note_id).await;

    let settings = store::load_or_bootstrap(&state, &user_id).await?;
    Ok(Json(SettingsResponse::from_settings(&settings)))
}

async fn put_settings(
    State(state): State<AppState>,
    RequireAuth(user_id): RequireAuth,
    WithRejection(Json(body), _): WithRejection<Json<PutSettingsRequest>, AppError>,
) -> AppResult<Json<SettingsResponse>> {
    if let Some(theme) = &body.theme {
        if !KNOWN_THEMES.contains(&theme.as_str()) {
            return Err(AppError::BadRequest("unknown theme".to_string()));
        }
    }
    if let Some(model) = &body.openrouter_model {
        if !is_valid_model_id(model) {
            return Err(AppError::BadRequest(
                "invalid OpenRouter model id".to_string(),
            ));
        }
    }
    if let Some(priority) = &body.notify_priority {
        if !is_valid_notify_priority(priority) {
            return Err(AppError::BadRequest(
                "notify_priority must be low/medium/high/critical".to_string(),
            ));
        }
    }
    if let Some(scope) = &body.notify_scope {
        if !is_valid_notify_scope(scope) {
            return Err(AppError::BadRequest(
                "notify_scope must be ours/theirs/both".to_string(),
            ));
        }
    }
    if let Some(schedule) = &body.notify_schedule {
        if !notify::is_valid_rrule(schedule) {
            return Err(AppError::BadRequest(
                "notify_schedule must be a valid RRULE".to_string(),
            ));
        }
    }
    if let Some(endpoint) = &body.notify_endpoint {
        if !notify::is_valid_endpoint(endpoint) {
            return Err(AppError::BadRequest(
                "notify_endpoint must be an http(s) URL".to_string(),
            ));
        }
    }
    if let Some(endpoint) = &body.ai_endpoint {
        if !notify::is_valid_endpoint(endpoint) {
            return Err(AppError::BadRequest(
                "ai_endpoint must be an http(s) URL".to_string(),
            ));
        }
    }

    let note_id = store::settings_note_id(&user_id);
    let rel_path = note_id_to_path(&note_id);
    let _guard = state.note_locks.lock(&note_id).await;

    // Idempotent bootstrap in case this is the very first write for this
    // user (e.g. the client PUTs before ever GETting).
    let _ = store::load_or_bootstrap(&state, &user_id).await?;

    let raw = state
        .notes_repo
        .read_file(&rel_path)
        .map_err(AppError::Internal)?
        .ok_or_else(|| {
            AppError::Internal(anyhow::anyhow!("settings note vanished after bootstrap"))
        })?;

    let mut fm = rust_note_core::frontmatter::Frontmatter::parse(&raw);
    if let Some(theme) = &body.theme {
        fm.set("theme", theme);
    }
    if let Some(model) = &body.openrouter_model {
        fm.set("openrouter_model", model);
    }
    // A key sent (even empty, to clear it) is written; omitted leaves it as-is.
    if let Some(key) = &body.openrouter_api_key {
        fm.set("openrouter_api_key", key);
    }
    if let Some(endpoint) = &body.ai_endpoint {
        fm.set("ai_endpoint", endpoint);
    }
    if let Some(enabled) = &body.notify_enabled {
        fm.set("notify_enabled", if *enabled { "true" } else { "false" });
    }
    if let Some(endpoint) = &body.notify_endpoint {
        fm.set("notify_endpoint", endpoint);
    }
    if let Some(priority) = &body.notify_priority {
        fm.set("notify_priority", priority);
    }
    if let Some(scope) = &body.notify_scope {
        fm.set("notify_scope", scope);
    }
    if let Some(schedule) = &body.notify_schedule {
        fm.set("notify_schedule", schedule);
    }
    // Write-only secret (even empty, to clear it); omitted leaves it as-is.
    if let Some(token) = &body.notify_token {
        fm.set("notify_token", token);
    }
    let new_content = fm.render();

    let (author_name, author_email) = crate::db_users::commit_author(&state.db, &user_id)
        .await
        .map_err(AppError::Internal)?;

    state
        .notes_repo
        .write_and_commit(
            &rel_path,
            &new_content,
            &author_name,
            &author_email,
            "update settings",
        )
        .await
        .map_err(AppError::Internal)?;

    acl::touch_updated_at(&state.db, &note_id)
        .await
        .map_err(AppError::Internal)?;

    // Re-read so the response reflects the persisted state (incl. has-key).
    let updated = store::parse_settings_tolerant(&new_content);
    Ok(Json(SettingsResponse::from_settings(&updated)))
}

/// Response of `GET /api/ai/models`: the sorted model ids available on the
/// user's AI endpoint, or an empty list plus a human-readable `error` when a
/// key is missing or the upstream can't be reached. Always HTTP 200 — never a
/// 500 — so the frontend can degrade gracefully. The API key is NEVER included.
#[derive(Debug, Serialize)]
struct AiModelsResponse {
    models: Vec<String>,
    error: Option<String>,
}

impl AiModelsResponse {
    fn ok(models: Vec<String>) -> Json<Self> {
        Json(Self {
            models,
            error: None,
        })
    }
    fn err(message: impl Into<String>) -> Json<Self> {
        Json(Self {
            models: Vec::new(),
            error: Some(message.into()),
        })
    }
}

/// OpenAI-compatible `GET {base}/models` envelope: `{ "data": [{ "id": … }] }`.
#[derive(Debug, Deserialize)]
struct ModelsEnvelope {
    data: Vec<ModelEntry>,
}
#[derive(Debug, Deserialize)]
struct ModelEntry {
    id: String,
}

/// `GET /api/ai/models` — proxy the user's AI endpoint `GET {ai_endpoint}/models`
/// with their Bearer key and return the sorted model ids. Degrades gracefully
/// (HTTP 200 with an `error` string, empty `models`) when no key is configured
/// or the upstream fails; never 500s and never leaks the key.
async fn get_ai_models(
    State(state): State<AppState>,
    RequireAuth(user_id): RequireAuth,
) -> Json<AiModelsResponse> {
    let note_id = store::settings_note_id(&user_id);
    let settings = {
        let _guard = state.note_locks.lock(&note_id).await;
        match store::load_or_bootstrap(&state, &user_id).await {
            Ok(s) => s,
            Err(_) => return AiModelsResponse::err("could not load settings"),
        }
    };

    // Resolve the key exactly like the query endpoint (settings, else env).
    let key = if !settings.openrouter_api_key.is_empty() {
        settings.openrouter_api_key.clone()
    } else if let Some(k) = &state.config.openrouter_api_key {
        k.clone()
    } else {
        return AiModelsResponse::err("set an API key first");
    };

    let url = format!(
        "{}/models",
        settings.ai_endpoint.trim().trim_end_matches('/')
    );

    let client = match crate::recurring::http_client() {
        Ok(c) => c,
        Err(_) => return AiModelsResponse::err("could not build HTTP client"),
    };

    // The key rides in the Authorization header (never the URL), so none of the
    // error strings below can leak it.
    let resp = match client.get(&url).bearer_auth(&key).send().await {
        Ok(r) => r,
        Err(_) => return AiModelsResponse::err("could not reach the AI endpoint"),
    };
    let status = resp.status();
    if !status.is_success() {
        return AiModelsResponse::err(format!("the AI endpoint returned {status}"));
    }
    let envelope: ModelsEnvelope = match resp.json().await {
        Ok(e) => e,
        Err(_) => return AiModelsResponse::err("could not parse the AI endpoint model list"),
    };

    let mut models: Vec<String> = envelope.data.into_iter().map(|m| m.id).collect();
    models.sort();
    AiModelsResponse::ok(models)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::oidc::OidcClient;
    use crate::config::Config;
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
    async fn get_settings_bootstraps_default_theme() {
        let (state, _notes_dir, _db_dir) = test_state().await;

        let resp = get_settings(State(state.clone()), RequireAuth("alice".to_string()))
            .await
            .unwrap()
            .0;
        assert_eq!(resp.theme, "ration");
    }

    #[tokio::test]
    async fn put_preserves_unknown_frontmatter_key_and_body_text() {
        let (state, _notes_dir, _db_dir) = test_state().await;

        let note_id = store::settings_note_id("alice");
        let rel_path = note_id_to_path(&note_id);

        acl::ensure_note_registered(&state.db, &note_id, "alice")
            .await
            .unwrap();
        state
            .notes_repo
            .write_and_commit(
                &rel_path,
                "---\ntheme: ration\nfuture_key: keep-me\n---\nSome body I typed.\n",
                "alice",
                "alice@example.com",
                "seed",
            )
            .await
            .unwrap();

        let resp = put_settings(
            State(state.clone()),
            RequireAuth("alice".to_string()),
            WithRejection(
                Json(PutSettingsRequest {
                    theme: Some("ration".to_string()),
                    ..Default::default()
                }),
                std::marker::PhantomData,
            ),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(resp.theme, "ration");

        let content = state.notes_repo.read_file(&rel_path).unwrap().unwrap();
        assert!(content.contains("future_key: keep-me"));
        assert!(content.contains("Some body I typed."));
    }

    #[tokio::test]
    async fn put_with_unknown_theme_is_rejected_with_no_new_commit() {
        let (state, _notes_dir, _db_dir) = test_state().await;

        let note_id = store::settings_note_id("alice");
        let rel_path = note_id_to_path(&note_id);

        // Bootstrap first so there's a baseline commit count to compare
        // against.
        let _ = get_settings(State(state.clone()), RequireAuth("alice".to_string()))
            .await
            .unwrap();
        let commits_before = state.notes_repo.history(&rel_path).await.unwrap().len();

        let err = put_settings(
            State(state.clone()),
            RequireAuth("alice".to_string()),
            WithRejection(
                Json(PutSettingsRequest {
                    theme: Some("nonexistent-theme".to_string()),
                    ..Default::default()
                }),
                std::marker::PhantomData,
            ),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, AppError::BadRequest(_)));

        let commits_after = state.notes_repo.history(&rel_path).await.unwrap().len();
        assert_eq!(
            commits_before, commits_after,
            "a rejected PUT must not create a new commit"
        );
    }

    #[tokio::test]
    async fn openrouter_key_is_write_only_and_model_round_trips() {
        let (state, _notes_dir, _db_dir) = test_state().await;
        let rel_path = note_id_to_path(&store::settings_note_id("alice"));

        let resp = put_settings(
            State(state.clone()),
            RequireAuth("alice".to_string()),
            WithRejection(
                Json(PutSettingsRequest {
                    openrouter_model: Some("anthropic/claude-3.5-haiku".to_string()),
                    openrouter_api_key: Some("sk-secret-123".to_string()),
                    ..Default::default()
                }),
                std::marker::PhantomData,
            ),
        )
        .await
        .unwrap()
        .0;

        // The GET-shaped response exposes the model and a boolean, never the key.
        assert_eq!(resp.openrouter_model, "anthropic/claude-3.5-haiku");
        assert!(resp.has_openrouter_key);
        let json = serde_json::to_string(&resp).unwrap();
        assert!(
            !json.contains("sk-secret-123"),
            "raw key must never be serialized"
        );

        // The key IS persisted server-side (readable by the query endpoint).
        let content = state.notes_repo.read_file(&rel_path).unwrap().unwrap();
        let parsed = store::parse_settings_tolerant(&content);
        assert_eq!(parsed.openrouter_api_key, "sk-secret-123");

        // A later PUT that omits the key leaves it intact.
        let resp2 = put_settings(
            State(state.clone()),
            RequireAuth("alice".to_string()),
            WithRejection(
                Json(PutSettingsRequest {
                    theme: Some("ration".to_string()),
                    ..Default::default()
                }),
                std::marker::PhantomData,
            ),
        )
        .await
        .unwrap()
        .0;
        assert!(resp2.has_openrouter_key, "omitted key must not be cleared");
    }

    #[tokio::test]
    async fn notify_fields_round_trip_and_token_is_write_only() {
        let (state, _notes_dir, _db_dir) = test_state().await;
        let rel_path = note_id_to_path(&store::settings_note_id("alice"));

        let resp = put_settings(
            State(state.clone()),
            RequireAuth("alice".to_string()),
            WithRejection(
                Json(PutSettingsRequest {
                    notify_enabled: Some(true),
                    notify_endpoint: Some("https://push.example.com".to_string()),
                    notify_priority: Some("critical".to_string()),
                    notify_scope: Some("ours".to_string()),
                    notify_schedule: Some("FREQ=DAILY;BYHOUR=18;BYMINUTE=0".to_string()),
                    notify_token: Some("ntfy-secret-xyz".to_string()),
                    ..Default::default()
                }),
                std::marker::PhantomData,
            ),
        )
        .await
        .unwrap()
        .0;

        // Non-secret fields surface; `has_notify_token` is true; the token itself
        // is never serialized.
        assert!(resp.notify_enabled);
        assert_eq!(resp.notify_endpoint, "https://push.example.com");
        assert_eq!(resp.notify_priority, "critical");
        assert_eq!(resp.notify_scope, "ours");
        assert_eq!(resp.notify_schedule, "FREQ=DAILY;BYHOUR=18;BYMINUTE=0");
        assert!(resp.has_notify_token);
        let json = serde_json::to_string(&resp).unwrap();
        assert!(
            !json.contains("ntfy-secret-xyz"),
            "raw notify token must never be serialized"
        );
        assert!(
            !json.contains("\"notify_token\""),
            "response must not carry a notify_token field (only has_notify_token)"
        );
        assert!(
            !json.contains("notify_last_sent"),
            "response must not expose notify_last_sent"
        );

        // The token IS persisted server-side.
        let content = state.notes_repo.read_file(&rel_path).unwrap().unwrap();
        let parsed = store::parse_settings_tolerant(&content);
        assert_eq!(parsed.notify_token, "ntfy-secret-xyz");

        // A later PUT omitting the token leaves it intact.
        let resp2 = put_settings(
            State(state.clone()),
            RequireAuth("alice".to_string()),
            WithRejection(
                Json(PutSettingsRequest {
                    notify_scope: Some("both".to_string()),
                    ..Default::default()
                }),
                std::marker::PhantomData,
            ),
        )
        .await
        .unwrap()
        .0;
        assert!(resp2.has_notify_token, "omitted token must not be cleared");
        assert_eq!(resp2.notify_scope, "both");
    }

    #[tokio::test]
    async fn ai_endpoint_round_trips_and_is_exposed() {
        let (state, _notes_dir, _db_dir) = test_state().await;

        // Default surfaces before any write.
        let resp = get_settings(State(state.clone()), RequireAuth("alice".to_string()))
            .await
            .unwrap()
            .0;
        assert_eq!(resp.ai_endpoint, "https://litellm.osmosis.page/v1");
        assert_eq!(resp.openrouter_model, "gpt-oss-20b");

        let resp = put_settings(
            State(state.clone()),
            RequireAuth("alice".to_string()),
            WithRejection(
                Json(PutSettingsRequest {
                    ai_endpoint: Some("https://proxy.example.com/v1".to_string()),
                    ..Default::default()
                }),
                std::marker::PhantomData,
            ),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(resp.ai_endpoint, "https://proxy.example.com/v1");
    }

    #[tokio::test]
    async fn put_rejects_invalid_ai_endpoint() {
        let (state, _notes_dir, _db_dir) = test_state().await;

        for bad in ["ftp://nope", "not-a-url", "https://"] {
            let err = put_settings(
                State(state.clone()),
                RequireAuth("alice".to_string()),
                WithRejection(
                    Json(PutSettingsRequest {
                        ai_endpoint: Some(bad.to_string()),
                        ..Default::default()
                    }),
                    std::marker::PhantomData,
                ),
            )
            .await
            .unwrap_err();
            assert!(
                matches!(err, AppError::BadRequest(_)),
                "{bad:?} must reject"
            );
        }
    }

    #[tokio::test]
    async fn put_rejects_invalid_notify_values() {
        let (state, _notes_dir, _db_dir) = test_state().await;

        for bad in [
            PutSettingsRequest {
                notify_priority: Some("urgent".to_string()),
                ..Default::default()
            },
            PutSettingsRequest {
                notify_scope: Some("all".to_string()),
                ..Default::default()
            },
            PutSettingsRequest {
                notify_schedule: Some("not-an-rrule".to_string()),
                ..Default::default()
            },
            PutSettingsRequest {
                notify_endpoint: Some("ftp://nope".to_string()),
                ..Default::default()
            },
        ] {
            let err = put_settings(
                State(state.clone()),
                RequireAuth("alice".to_string()),
                WithRejection(Json(bad), std::marker::PhantomData),
            )
            .await
            .unwrap_err();
            assert!(matches!(err, AppError::BadRequest(_)));
        }
    }

    #[tokio::test]
    async fn get_on_corrupted_yaml_returns_default_and_does_not_rewrite_file() {
        let (state, _notes_dir, _db_dir) = test_state().await;

        let note_id = store::settings_note_id("alice");
        let rel_path = note_id_to_path(&note_id);

        acl::ensure_note_registered(&state.db, &note_id, "alice")
            .await
            .unwrap();
        state
            .notes_repo
            .write_and_commit(
                &rel_path,
                "not frontmatter at all",
                "alice",
                "alice@example.com",
                "seed corrupt",
            )
            .await
            .unwrap();

        let resp = get_settings(State(state.clone()), RequireAuth("alice".to_string()))
            .await
            .unwrap()
            .0;
        assert_eq!(resp.theme, "ration");

        let content_after = state.notes_repo.read_file(&rel_path).unwrap().unwrap();
        assert_eq!(
            content_after, "not frontmatter at all",
            "GET must not rewrite the file even when its content is corrupt"
        );
    }
}
