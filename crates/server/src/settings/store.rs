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
}

impl Default for UserSettings {
    fn default() -> Self {
        Self {
            theme: "ration".to_string(),
        }
    }
}

pub const KNOWN_THEMES: &[&str] = &["ration"];

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
        body: "# Settings\n\n\
            This note stores your personal rust_note settings as YAML frontmatter \
            above.\nYou can edit it by hand — just keep the `---` fences and valid \
            `key: value`\nlines. Fields the app doesn't recognize (yet) are \
            preserved as-is.\n"
            .to_string(),
    };
    fm.set("theme", &UserSettings::default().theme);
    fm.render()
}

/// Parse settings from raw note content, tolerating missing/invalid `theme`
/// (falls back to the default) rather than erroring.
pub fn parse_settings_tolerant(content: &str) -> UserSettings {
    let fm = Frontmatter::parse(content);
    let theme = fm
        .get("theme")
        .filter(|t| KNOWN_THEMES.contains(t))
        .unwrap_or("ration")
        .to_string();
    UserSettings { theme }
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
}
