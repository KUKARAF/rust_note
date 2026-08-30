//! Per-user settings REST routes (`GET`/`PUT /api/settings`).

use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use axum_extra::extract::WithRejection;
use serde::{Deserialize, Serialize};

use super::store::{self, is_valid_model_id, UserSettings, KNOWN_THEMES};
use crate::auth::session::RequireAuth;
use crate::error::{AppError, AppResult};
use crate::notes::{acl, fs_store::note_id_to_path};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new().route("/api/settings", get(get_settings).put(put_settings))
}

#[derive(Debug, Serialize)]
struct SettingsResponse {
    theme: String,
    openrouter_model: String,
    /// Whether an OpenRouter API key is stored. The key itself is NEVER
    /// returned — the client only needs to know if one is set.
    has_openrouter_key: bool,
}

impl SettingsResponse {
    fn from_settings(s: &UserSettings) -> Self {
        Self {
            theme: s.theme.clone(),
            openrouter_model: s.openrouter_model.clone(),
            has_openrouter_key: !s.openrouter_api_key.is_empty(),
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
