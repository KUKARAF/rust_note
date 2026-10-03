//! Per-user settings note: read/parse/bootstrap logic, backed by the same
//! git-backed `NotesRepo` as ordinary notes (not SQLite).

use rust_note_core::frontmatter::Frontmatter;
use serde::{Deserialize, Serialize};

use crate::db_users::commit_author;
use crate::error::{AppError, AppResult};
use crate::notes::acl;
use crate::notes::fs_store::note_id_to_path;
use crate::state::AppState;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UserSettings {
    pub theme: String,
    /// OpenRouter model id used for the natural-language todo query, e.g.
    /// `openai/gpt-4o-mini`.
    pub openrouter_model: String,
    /// Model id used by the in-note `#AI!` inline-edit command (see
    /// `docs/ai-command.md`). Separate from `openrouter_model` so the cheap,
    /// fast model that drives live edits can differ from the todo-query model.
    pub ai_command_model: String,
    /// OpenRouter API key. Stored here (server-side) and never returned by the
    /// settings GET — see `SettingsResponse`.
    pub openrouter_api_key: String,
    /// Base URL of the OpenAI-compatible AI endpoint (the user's LiteLLM
    /// proxy). `/chat/completions` and `/models` are appended to it. See
    /// `docs/litellm.md`.
    pub ai_endpoint: String,

    // ---- priority-notify digest (see `docs/notifications.md`) ----
    /// Whether the overdue-pipeline digest is pushed to priority-notify.
    pub notify_enabled: bool,
    /// Base URL of the priority-notify server; `/api/notifications/` is
    /// appended when POSTing.
    pub notify_endpoint: String,
    /// priority-notify API token. A **write-only secret** (like
    /// `openrouter_api_key`): stored server-side, never returned by the
    /// settings GET and never logged.
    pub notify_token: String,
    /// Notification priority: one of low/medium/high/critical.
    pub notify_priority: String,
    /// Which overdue items to include: one of ours/theirs/both.
    pub notify_scope: String,
    /// RRULE (iCal) governing when the digest fires, evaluated in
    /// [`crate::stats::DEFAULT_TZ`].
    pub notify_schedule: String,
    /// Internal bookkeeping: RFC3339 timestamp of the last fire, stamped by
    /// the scheduler. Not user-editable and never returned to the client.
    pub notify_last_sent: String,
}

impl Default for UserSettings {
    fn default() -> Self {
        Self {
            theme: "ration".to_string(),
            openrouter_model: DEFAULT_OPENROUTER_MODEL.to_string(),
            ai_command_model: DEFAULT_AI_COMMAND_MODEL.to_string(),
            openrouter_api_key: String::new(),
            ai_endpoint: DEFAULT_AI_ENDPOINT.to_string(),
            notify_enabled: false,
            notify_endpoint: DEFAULT_NOTIFY_ENDPOINT.to_string(),
            notify_token: String::new(),
            notify_priority: DEFAULT_NOTIFY_PRIORITY.to_string(),
            notify_scope: DEFAULT_NOTIFY_SCOPE.to_string(),
            notify_schedule: DEFAULT_NOTIFY_SCHEDULE.to_string(),
            notify_last_sent: String::new(),
        }
    }
}

pub const KNOWN_THEMES: &[&str] = &["ration"];

/// Default priority-notify base URL.
pub const DEFAULT_NOTIFY_ENDPOINT: &str = "https://notifications.osmosis.page";
/// Default notification priority.
pub const DEFAULT_NOTIFY_PRIORITY: &str = "high";
/// Default overdue scope.
pub const DEFAULT_NOTIFY_SCOPE: &str = "both";
/// Default fire schedule: daily at 08:00 (Europe/Warsaw).
pub const DEFAULT_NOTIFY_SCHEDULE: &str = "FREQ=DAILY;BYHOUR=8;BYMINUTE=0";

/// Accepted `notify_priority` values (priority-notify's levels).
pub const NOTIFY_PRIORITIES: &[&str] = &["low", "medium", "high", "critical"];
/// Accepted `notify_scope` values.
pub const NOTIFY_SCOPES: &[&str] = &["ours", "theirs", "both"];

/// Whether `s` is a valid `notify_priority`.
pub fn is_valid_notify_priority(s: &str) -> bool {
    NOTIFY_PRIORITIES.contains(&s)
}

/// Whether `s` is a valid `notify_scope`.
pub fn is_valid_notify_scope(s: &str) -> bool {
    NOTIFY_SCOPES.contains(&s)
}

/// Default AI (LiteLLM) base URL for new settings; `/chat/completions` and
/// `/models` are appended to it. See `docs/litellm.md`.
pub const DEFAULT_AI_ENDPOINT: &str = "https://litellm.osmosis.page/v1";

/// Default model for new settings (a local model on the LiteLLM proxy). The
/// settings UI lets the user pick from the proxy's model list;
/// [`is_valid_model_id`] (not an allowlist) governs what's accepted, since the
/// catalog changes over time.
pub const DEFAULT_OPENROUTER_MODEL: &str = "gpt-oss-20b";

/// Default model for the in-note `#AI!` inline-edit command. A small, fast
/// model (benchmarked clean + ~1s latency); see `docs/ai-command.md`. Governed
/// by [`is_valid_model_id`] (format, not an allowlist) like `openrouter_model`.
pub const DEFAULT_AI_COMMAND_MODEL: &str = "openrouter/google/gemini-3.1-flash-lite";

/// Whether `model` is a plausible OpenRouter model id: non-empty, bounded, and
/// only `vendor/model`-style characters. Deliberately permissive (format, not
/// membership) so new models aren't rejected.
pub fn is_valid_model_id(model: &str) -> bool {
    !model.is_empty()
        && model.len() <= 100
        && model
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '-' | '_' | ':'))
}

/// The (git-backed) note id under which `user_id`'s settings are stored.
/// Distinct per user, so two users' settings notes never collide and are
/// governed by the same ACL as any other note (i.e. nobody but the owner
/// gets a `note_access` grant on it, so it stays private).
pub fn settings_note_id(user_id: &str) -> String {
    format!("_settings/{user_id}/main")
}

/// Default content for a brand-new settings note: `theme: ration`
/// frontmatter plus a `# Settings` heading (so `extract_title` and the
/// ordinary notes list treat it like any other note) and a short
/// explanation of the file for anyone who opens it directly.
fn default_note_content() -> String {
    let mut fm = Frontmatter {
        fields: Vec::new(),
        raw_extra: Vec::new(),
        body: "# Settings\n\n\
            This note stores your personal rust_note settings as YAML frontmatter \
            above.\nYou can edit it by hand — just keep the `---` fences and valid \
            `key: value`\nlines. Fields the app doesn't recognize (yet) are \
            preserved as-is.\n"
            .to_string(),
    };
    fm.set("theme", &UserSettings::default().theme);
    fm.set("openrouter_model", DEFAULT_OPENROUTER_MODEL);
    fm.set("ai_command_model", DEFAULT_AI_COMMAND_MODEL);
    fm.render()
}

/// Parse settings from raw note content, tolerating missing/invalid fields
/// (each falls back to its default) rather than erroring.
pub fn parse_settings_tolerant(content: &str) -> UserSettings {
    let fm = Frontmatter::parse(content);
    let theme = fm
        .get("theme")
        .filter(|t| KNOWN_THEMES.contains(t))
        .unwrap_or("ration")
        .to_string();
    let openrouter_model = fm
        .get("openrouter_model")
        .filter(|m| is_valid_model_id(m))
        .unwrap_or(DEFAULT_OPENROUTER_MODEL)
        .to_string();
    let ai_command_model = fm
        .get("ai_command_model")
        .filter(|m| is_valid_model_id(m))
        .unwrap_or(DEFAULT_AI_COMMAND_MODEL)
        .to_string();
    let openrouter_api_key = fm.get("openrouter_api_key").unwrap_or("").to_string();
    let ai_endpoint = fm
        .get("ai_endpoint")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(DEFAULT_AI_ENDPOINT)
        .to_string();

    // "true"/"false" tolerant: any value other than the literal `true` is off.
    let notify_enabled = fm.get("notify_enabled") == Some("true");
    let notify_endpoint = fm
        .get("notify_endpoint")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(DEFAULT_NOTIFY_ENDPOINT)
        .to_string();
    let notify_token = fm.get("notify_token").unwrap_or("").to_string();
    let notify_priority = fm
        .get("notify_priority")
        .map(str::trim)
        .filter(|s| is_valid_notify_priority(s))
        .unwrap_or(DEFAULT_NOTIFY_PRIORITY)
        .to_string();
    let notify_scope = fm
        .get("notify_scope")
        .map(str::trim)
        .filter(|s| is_valid_notify_scope(s))
        .unwrap_or(DEFAULT_NOTIFY_SCOPE)
        .to_string();
    let notify_schedule = fm
        .get("notify_schedule")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(DEFAULT_NOTIFY_SCHEDULE)
        .to_string();
    let notify_last_sent = fm.get("notify_last_sent").unwrap_or("").to_string();

    UserSettings {
        theme,
        openrouter_model,
        ai_command_model,
        openrouter_api_key,
        ai_endpoint,
        notify_enabled,
        notify_endpoint,
        notify_token,
        notify_priority,
        notify_scope,
        notify_schedule,
        notify_last_sent,
    }
}

/// Persist one or more frontmatter fields on `user_id`'s settings note,
/// read-modify-writing exactly like `put_settings` (bootstrap → parse → `set`
/// → write+commit → touch). Takes the settings-note lock internally.
///
/// Used by the notify scheduler to stamp `notify_last_sent` after a fire.
pub async fn update_settings_fields(
    state: &AppState,
    user_id: &str,
    fields: &[(&str, &str)],
) -> AppResult<()> {
    let note_id = settings_note_id(user_id);
    let rel_path = note_id_to_path(&note_id);
    let _guard = state.note_locks.lock(&note_id).await;

    load_or_bootstrap(state, user_id).await?;

    let raw = state
        .notes_repo
        .read_file(&rel_path)
        .map_err(AppError::Internal)?
        .ok_or_else(|| {
            AppError::Internal(anyhow::anyhow!("settings note vanished after bootstrap"))
        })?;

    let mut fm = Frontmatter::parse(&raw);
    for (key, value) in fields {
        fm.set(key, value);
    }
    let new_content = fm.render();

    let (author_name, author_email) = commit_author(&state.db, user_id)
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

    Ok(())
}

/// Load `user_id`'s settings note, creating it with defaults (registering
/// ownership + committing the default file) if it doesn't exist yet.
///
/// Caller must hold `state.note_locks.lock(&settings_note_id(user_id))`
/// across this call (and any subsequent read-modify-write) so a concurrent
/// request for the same user's settings note can't race the bootstrap.
pub async fn load_or_bootstrap(state: &AppState, user_id: &str) -> AppResult<UserSettings> {
    let note_id = settings_note_id(user_id);
    let rel_path = note_id_to_path(&note_id);

    let existing = state
        .notes_repo
        .read_file(&rel_path)
        .map_err(AppError::Internal)?;

    let content = match existing {
        Some(content) => content,
        None => {
            acl::ensure_note_registered(&state.db, &note_id, user_id)
                .await
                .map_err(AppError::Internal)?;

            let (author_name, author_email) = commit_author(&state.db, user_id)
                .await
                .map_err(AppError::Internal)?;
            let default_content = default_note_content();

            state
                .notes_repo
                .write_and_commit(
                    &rel_path,
                    &default_content,
                    &author_name,
                    &author_email,
                    "initialize settings",
                )
                .await
                .map_err(AppError::Internal)?;

            acl::touch_updated_at(&state.db, &note_id)
                .await
                .map_err(AppError::Internal)?;

            default_content
        }
    };

    Ok(parse_settings_tolerant(&content))
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
    async fn bootstrap_creates_default_settings_note_and_registers_ownership() {
        let (state, _notes_dir, _db_dir) = test_state().await;

        let settings = load_or_bootstrap(&state, "alice").await.unwrap();
        assert_eq!(settings.theme, "ration");

        let note_id = settings_note_id("alice");
        assert!(acl::is_owner(&state.db, &note_id, "alice").await.unwrap());

        // Confirmation test: the bootstrapped note is visible via the
        // ordinary notes list for its owner (exercises the real list
        // handler, not just the DB row).
        let listed = crate::notes::routes::list_notes(
            axum::extract::State(state.clone()),
            crate::auth::session::RequireAuth("alice".to_string()),
        )
        .await
        .unwrap()
        .0;
        assert!(
            listed.iter().any(|m| m.id == note_id),
            "settings note must appear in alice's note list; got {listed:?}"
        );
    }

    #[tokio::test]
    async fn bootstrap_is_idempotent() {
        let (state, _notes_dir, _db_dir) = test_state().await;

        let note_id = settings_note_id("alice");
        let rel_path = note_id_to_path(&note_id);

        let _ = load_or_bootstrap(&state, "alice").await.unwrap();
        let content_after_first = state.notes_repo.read_file(&rel_path).unwrap().unwrap();
        let commits_after_first = state.notes_repo.history(&rel_path).await.unwrap().len();

        let _ = load_or_bootstrap(&state, "alice").await.unwrap();
        let content_after_second = state.notes_repo.read_file(&rel_path).unwrap().unwrap();
        let commits_after_second = state.notes_repo.history(&rel_path).await.unwrap().len();

        assert_eq!(content_after_first, content_after_second);
        assert_eq!(
            commits_after_first, commits_after_second,
            "second load_or_bootstrap must not create a second commit"
        );
    }

    #[tokio::test]
    async fn cross_user_settings_notes_are_isolated() {
        let (state, _notes_dir, _db_dir) = test_state().await;

        let alice_id = settings_note_id("alice");
        let bob_id = settings_note_id("bob");
        assert_ne!(alice_id, bob_id);

        let _ = load_or_bootstrap(&state, "alice").await.unwrap();

        // Bob has no grant on alice's settings note.
        assert!(!acl::can_read(&state.db, &alice_id, "bob").await.unwrap());
    }

    #[test]
    fn default_note_content_round_trips_through_frontmatter_parse() {
        let content = default_note_content();
        let settings = parse_settings_tolerant(&content);
        assert_eq!(settings.theme, "ration");
        assert!(content.contains("# Settings"));
    }

    #[test]
    fn parse_settings_tolerant_falls_back_on_corrupt_yaml() {
        let settings = parse_settings_tolerant("not frontmatter at all");
        assert_eq!(settings.theme, "ration");
    }

    #[test]
    fn parse_settings_tolerant_falls_back_on_unknown_theme() {
        let settings = parse_settings_tolerant("---\ntheme: bogus-theme\n---\nbody\n");
        assert_eq!(settings.theme, "ration");
    }

    #[test]
    fn ai_endpoint_and_default_model_default_when_absent() {
        let s = parse_settings_tolerant("---\ntheme: ration\n---\n");
        assert_eq!(s.ai_endpoint, DEFAULT_AI_ENDPOINT);
        assert_eq!(s.ai_endpoint, "https://litellm.osmosis.page/v1");
        // The default model is now a LiteLLM-proxy local model.
        assert_eq!(s.openrouter_model, "gpt-oss-20b");
        assert_eq!(UserSettings::default().openrouter_model, "gpt-oss-20b");
        assert_eq!(UserSettings::default().ai_endpoint, DEFAULT_AI_ENDPOINT);
    }

    #[test]
    fn ai_command_model_defaults_and_round_trips() {
        // Absent -> default.
        let s = parse_settings_tolerant("---\ntheme: ration\n---\n");
        assert_eq!(s.ai_command_model, DEFAULT_AI_COMMAND_MODEL);
        assert_eq!(
            UserSettings::default().ai_command_model,
            DEFAULT_AI_COMMAND_MODEL
        );

        // Present + valid -> preserved.
        let s = parse_settings_tolerant(
            "---\ntheme: ration\nai_command_model: openai/gpt-4o-mini\n---\n",
        );
        assert_eq!(s.ai_command_model, "openai/gpt-4o-mini");

        // Present but invalid -> falls back to default.
        let s =
            parse_settings_tolerant("---\ntheme: ration\nai_command_model: \"has spaces!\"\n---\n");
        assert_eq!(s.ai_command_model, DEFAULT_AI_COMMAND_MODEL);
    }

    #[test]
    fn ai_endpoint_round_trips_through_frontmatter() {
        let s = parse_settings_tolerant(
            "---\ntheme: ration\nai_endpoint: https://proxy.example.com/v1\n---\nbody\n",
        );
        assert_eq!(s.ai_endpoint, "https://proxy.example.com/v1");
    }

    #[test]
    fn ai_endpoint_blank_falls_back_to_default() {
        let s = parse_settings_tolerant("---\ntheme: ration\nai_endpoint: \"   \"\n---\n");
        assert_eq!(s.ai_endpoint, DEFAULT_AI_ENDPOINT);
    }

    #[test]
    fn notify_fields_default_when_absent() {
        let s = parse_settings_tolerant("---\ntheme: ration\n---\n");
        assert!(!s.notify_enabled);
        assert_eq!(s.notify_endpoint, DEFAULT_NOTIFY_ENDPOINT);
        assert_eq!(s.notify_priority, "high");
        assert_eq!(s.notify_scope, "both");
        assert_eq!(s.notify_schedule, DEFAULT_NOTIFY_SCHEDULE);
        assert!(s.notify_token.is_empty());
        assert!(s.notify_last_sent.is_empty());
    }

    #[test]
    fn notify_fields_round_trip_through_frontmatter() {
        let content = "---\n\
            theme: ration\n\
            notify_enabled: true\n\
            notify_endpoint: https://push.example.com\n\
            notify_token: ntfy-secret-xyz\n\
            notify_priority: critical\n\
            notify_scope: ours\n\
            notify_schedule: FREQ=WEEKLY;BYDAY=MO;BYHOUR=9;BYMINUTE=0\n\
            notify_last_sent: 2026-10-01T06:00:00Z\n\
            ---\nbody\n";
        let s = parse_settings_tolerant(content);
        assert!(s.notify_enabled);
        assert_eq!(s.notify_endpoint, "https://push.example.com");
        assert_eq!(s.notify_token, "ntfy-secret-xyz");
        assert_eq!(s.notify_priority, "critical");
        assert_eq!(s.notify_scope, "ours");
        assert_eq!(
            s.notify_schedule,
            "FREQ=WEEKLY;BYDAY=MO;BYHOUR=9;BYMINUTE=0"
        );
        assert_eq!(s.notify_last_sent, "2026-10-01T06:00:00Z");
    }

    #[test]
    fn notify_invalid_enum_values_fall_back_to_defaults() {
        let s = parse_settings_tolerant(
            "---\ntheme: ration\nnotify_priority: urgent\nnotify_scope: everything\n---\n",
        );
        assert_eq!(s.notify_priority, "high");
        assert_eq!(s.notify_scope, "both");
    }

    #[test]
    fn notify_enabled_is_off_for_non_true_values() {
        for v in ["false", "yes", "1", "TRUE", ""] {
            let s = parse_settings_tolerant(&format!("---\nnotify_enabled: {v}\n---\n"));
            assert!(!s.notify_enabled, "{v:?} must not enable notify");
        }
    }
}
