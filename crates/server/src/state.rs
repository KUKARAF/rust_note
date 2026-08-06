//! Shared application state handed to Axum handlers via `axum::extract::State`.

use std::hash::{Hash, Hasher};
use std::sync::Arc;

use axum::extract::FromRef;
use axum_extra::extract::cookie::Key;
use sqlx::SqlitePool;

use crate::auth::oidc::OidcClient;
use crate::collab::room::RoomRegistry;
use crate::config::Config;
use crate::notes::repo::NotesRepo;

/// Fixed-size striped lock table used to serialize a whole note-id's
/// "check state -> mutate git -> mutate DB" critical section across
/// concurrent requests (e.g. a PUT racing a DELETE on the same note id).
///
/// `NotesRepo`'s internal mutex only serializes the git working-tree
/// write/commit itself; it says nothing about the `notes` DB row that
/// route handlers read and update around that call. Without this, a PUT
/// and a DELETE for the same note id can interleave their DB and
/// filesystem/git effects independently, leaving the DB and the git repo
/// disagreeing about whether the note exists. Striping (rather than one
/// lock per note id) keeps the table a fixed size regardless of how many
/// distinct note ids are ever touched.
const NOTE_LOCK_STRIPES: usize = 64;

#[derive(Clone)]
pub struct NoteLocks(Arc<[Arc<tokio::sync::Mutex<()>>; NOTE_LOCK_STRIPES]>);

impl NoteLocks {
    pub fn new() -> Self {
        Self(Arc::new(std::array::from_fn(|_| {
            Arc::new(tokio::sync::Mutex::new(()))
        })))
    }

    fn stripe_for(&self, note_id: &str) -> usize {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        note_id.hash(&mut hasher);
        (hasher.finish() as usize) % NOTE_LOCK_STRIPES
    }

    /// Acquire the lock stripe for `note_id`. Hold the returned guard for
    /// the entire existence-check + git-commit + DB-update sequence of a
    /// create/update/delete so those steps act as one atomic unit with
    /// respect to concurrent requests for the same note id.
    pub async fn lock(&self, note_id: &str) -> tokio::sync::OwnedMutexGuard<()> {
        let idx = self.stripe_for(note_id);
        // Provably in-bounds: `stripe_for` returns `hash % NOTE_LOCK_STRIPES`,
        // the same constant as this array's length, so `idx` is always
        // `0..NOTE_LOCK_STRIPES`. Clippy can't see that modulo invariant.
        #[allow(clippy::indexing_slicing)]
        let stripe = &self.0[idx];
        stripe.clone().lock_owned().await
    }
}

impl Default for NoteLocks {
    fn default() -> Self {
        Self::new()
    }
}

// `NotesRepo` is already cheaply `Clone` (it's an `Arc<Mutex<git2::Repository>>`
// internally), so `AppState` itself just needs to be `Clone` for
// `axum::extract::State` to work.
#[derive(Clone)]
pub struct AppState {
    pub db: SqlitePool,
    pub notes_repo: NotesRepo,
    pub config: Arc<Config>,
    /// `None` if OIDC discovery failed at startup (see `main.rs`) - OIDC
    /// routes return a clear 500 rather than panicking in that case.
    pub oidc: Option<Arc<OidcClient>>,
    /// Key used to sign/encrypt the short-lived OIDC login-flow cookie and
    /// (in a later phase) the share-link guest identity cookie.
    pub cookie_key: Key,
    /// Per-note-id striped locks; see [`NoteLocks`].
    pub note_locks: NoteLocks,
    /// Live collaborative-editing rooms, keyed by note id. Cheap to clone
    /// (an `Arc<DashMap>` inside); see [`RoomRegistry`].
    pub rooms: RoomRegistry,
}

impl FromRef<AppState> for Key {
    fn from_ref(state: &AppState) -> Self {
        state.cookie_key.clone()
    }
}
