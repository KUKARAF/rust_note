//! Real-time collaborative editing (Phase 2, CRDT-based).
//!
//! The CRDT stack is a single crate, `yrs` 0.26, whose in-crate
//! [`yrs::sync`] module provides both the Yjs document-sync and awareness
//! (presence) protocols, wire-compatible with the JS `y-websocket` client.
//! See [`protocol`] for the concrete API surface and proof tests.
//!
//! The pieces:
//! * [`room`] — the [`room::RoomRegistry`] + per-note [`room::Room`] (the
//!   authoritative CRDT doc, presence, and broadcast fan-out).
//! * [`ws`] — the authenticated `GET /ws/notes/{*note_id}` handler.
//! * [`persist`] — debounced git persistence of collab edits.
//! * [`protocol`] — the spike's API/wire-format reference + proof tests.
//!
//! The route is mounted by [`crate::routes::build`]; a graceful-shutdown
//! flush of all live rooms is wired in [`crate`] (`main`).

pub mod persist;
pub mod protocol;
pub mod room;
pub mod ws;

#[cfg(test)]
pub mod test_support {
    //! Shared test fixtures for the collab unit tests (a fully-formed
    //! [`AppState`] backed by tempdir git + sqlite, with the dev `admin`
    //! user and its notes registered as they would be at startup).

    use std::sync::Arc;

    use axum_extra::extract::cookie::Key;

    use crate::auth::oidc::OidcClient;
    use crate::config::Config;
    use crate::notes::repo::NotesRepo;
    use crate::state::{AppState, NoteLocks};

    /// Build an `AppState` on throwaway tempdirs. The returned `TempDir`s
    /// must be kept alive for the duration of the test.
    pub async fn test_state() -> (AppState, tempfile::TempDir, tempfile::TempDir) {
        let notes_dir = tempfile::tempdir().unwrap();
        let db_dir = tempfile::tempdir().unwrap();
        let db_path = db_dir.path().join("test.db");

        let notes_repo = NotesRepo::open_or_init(notes_dir.path().to_str().unwrap()).unwrap();
        let db = crate::db::init_pool(db_path.to_str().unwrap())
            .await
            .unwrap();

        // The dev user, so `effective_user_id`/ACL checks resolve as at runtime.
        sqlx::query("INSERT INTO users (id, email, display_name, created_at) VALUES (?, ?, ?, ?)")
            .bind("admin")
            .bind("admin@localhost")
            .bind("Admin")
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
            note_locks: NoteLocks::new(),
            rooms: crate::collab::room::RoomRegistry::new(),
        };

        (state, notes_dir, db_dir)
    }
}
