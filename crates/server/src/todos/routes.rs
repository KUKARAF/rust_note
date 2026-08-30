//! `GET /api/todos` — aggregate checkbox tasks across daily notes.

use axum::extract::{Query, State};
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use rust_note_core::tasks::{parse_tasks, Task};

use crate::auth::session::RequireAuth;
use crate::error::{AppError, AppResult};
use crate::notes::acl;
use crate::notes::fs_store::{is_valid_note_id, path_to_note_id};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new().route("/api/todos", get(list_todos))
}

/// One parsed task plus the note it came from. The task fields are flattened
/// so the wire shape is a flat object (`{ note_id, date, line, done, ... }`).
#[derive(Debug, Serialize)]
struct Todo {
    /// Source note id, e.g. `diary/2026-08-13`.
    note_id: String,
    /// The date encoded in the daily-note id (`YYYY-MM-DD`), when applicable.
    date: Option<String>,
    #[serde(flatten)]
    task: Task,
}

#[derive(Debug, Deserialize)]
struct TodosQuery {
    /// `diary` (default) restricts to daily notes; `all` scans every note.
    #[serde(default)]
    scope: Scope,
    /// Include completed (`[x]`) tasks. Defaults to true — the board shows
    /// done tasks dimmed rather than hiding them.
    #[serde(default = "default_true")]
    include_done: bool,
}

#[derive(Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
enum Scope {
    #[default]
    Diary,
    All,
}

fn default_true() -> bool {
    true
}

/// If `note_id` is a daily note (`diary/YYYY-MM-DD`), return its date part.
fn diary_date(note_id: &str) -> Option<String> {
    let rest = note_id.strip_prefix("diary/")?;
    // Must be exactly `YYYY-MM-DD` with no further path segments.
    if rest.len() != 10 || rest.contains('/') {
        return None;
    }
    let shaped = rest.as_bytes().iter().enumerate().all(|(i, &b)| match i {
        4 | 7 => b == b'-',
        _ => b.is_ascii_digit(),
    });
    shaped.then(|| rest.to_string())
}

async fn list_todos(
    State(state): State<AppState>,
    RequireAuth(user_id): RequireAuth,
    Query(query): Query<TodosQuery>,
) -> AppResult<Json<Vec<Todo>>> {
    let paths = state.notes_repo.list_notes().map_err(AppError::Internal)?;

    let mut todos = Vec::new();
    for rel_path in paths {
        let note_id = path_to_note_id(&rel_path);
        if !is_valid_note_id(&note_id) {
            continue;
        }

        let date = diary_date(&note_id);
        if query.scope == Scope::Diary && date.is_none() {
            continue;
        }
        // Drawings are JSON, never task-bearing markdown.
        if note_id.ends_with(".excalidraw") {
            continue;
        }

        // Same lazy-adopt + ACL gate as the notes list, so externally-created
        // daily notes (Obsidian/Syncthing) are included and other users'
        // notes are filtered out.
        acl::adopt_if_orphaned(&state.db, &note_id, &user_id)
            .await
            .map_err(AppError::Internal)?;
        if !acl::can_read(&state.db, &note_id, &user_id)
            .await
            .map_err(AppError::Internal)?
        {
            continue;
        }

        // Prefer a live collab room's in-memory text over disk: a note open in
        // the editor is only flushed to disk after a debounce, so disk can lag
        // by seconds. The room's snapshot is the source of truth when present.
        let content = match state.rooms.get(&note_id) {
            Some(room) => room.snapshot_text(),
            None => state
                .notes_repo
                .read_file(&rel_path)
                .map_err(AppError::Internal)?
                .unwrap_or_default(),
        };

        for task in parse_tasks(&content) {
            if !query.include_done && task.done {
                continue;
            }
            todos.push(Todo {
                note_id: note_id.clone(),
                date: date.clone(),
                task,
            });
        }
    }

    Ok(Json(todos))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::oidc::OidcClient;
    use crate::config::Config;
    use crate::notes::fs_store::note_id_to_path;
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
        sqlx::query(
            "INSERT INTO users (id, email, display_name, created_at) \
             VALUES ('alice', 'alice@example.com', 'Alice', '2026-01-01T00:00:00Z')",
        )
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

    async fn seed(state: &AppState, note_id: &str, content: &str) {
        let rel = note_id_to_path(note_id);
        acl::ensure_note_registered(&state.db, note_id, "alice")
            .await
            .unwrap();
        state
            .notes_repo
            .write_and_commit(&rel, content, "alice", "alice@example.com", "seed")
            .await
            .unwrap();
    }

    #[test]
    fn diary_date_matches_only_daily_notes() {
        assert_eq!(
            diary_date("diary/2026-08-13").as_deref(),
            Some("2026-08-13")
        );
        assert_eq!(diary_date("diary/2026-8-13"), None);
        assert_eq!(diary_date("diary/2026-08-13/extra"), None);
        assert_eq!(diary_date("notes/foo"), None);
        assert_eq!(diary_date("diary/notes"), None);
    }

    #[tokio::test]
    async fn aggregates_tasks_from_daily_notes_only() {
        let (state, _n, _d) = test_state().await;
        seed(
            &state,
            "diary/2026-08-13",
            "---\nplan: true\n---\n- [ ] implement dependabot start:12:00 p:1 #fb\n- [x] done thing\n",
        )
        .await;
        // A non-diary note with a task must be excluded under scope=diary.
        seed(&state, "projects/rust-note", "- [ ] not a diary task\n").await;

        let todos = list_todos(
            State(state.clone()),
            RequireAuth("alice".to_string()),
            Query(TodosQuery {
                scope: Scope::Diary,
                include_done: true,
            }),
        )
        .await
        .unwrap()
        .0;

        assert_eq!(todos.len(), 2, "both diary tasks, no project task");
        assert!(todos.iter().all(|t| t.note_id == "diary/2026-08-13"));
        let open = todos.iter().find(|t| !t.task.done).unwrap();
        assert_eq!(open.date.as_deref(), Some("2026-08-13"));
        assert_eq!(open.task.pomodoros, Some(1));
        assert_eq!(open.task.start.as_deref(), Some("12:00"));
        assert_eq!(
            open.task.burner,
            Some(rust_note_core::tasks::Burner::Frontburner)
        );
    }

    #[tokio::test]
    async fn include_done_false_hides_completed() {
        let (state, _n, _d) = test_state().await;
        seed(
            &state,
            "diary/2026-08-13",
            "- [ ] open one\n- [x] closed one\n",
        )
        .await;

        let todos = list_todos(
            State(state.clone()),
            RequireAuth("alice".to_string()),
            Query(TodosQuery {
                scope: Scope::Diary,
                include_done: false,
            }),
        )
        .await
        .unwrap()
        .0;

        assert_eq!(todos.len(), 1);
        assert!(!todos[0].task.done);
    }

    #[tokio::test]
    async fn scope_all_includes_non_diary_notes() {
        let (state, _n, _d) = test_state().await;
        seed(&state, "diary/2026-08-13", "- [ ] diary task\n").await;
        seed(&state, "projects/rust-note", "- [ ] project task\n").await;

        let todos = list_todos(
            State(state.clone()),
            RequireAuth("alice".to_string()),
            Query(TodosQuery {
                scope: Scope::All,
                include_done: true,
            }),
        )
        .await
        .unwrap()
        .0;

        assert_eq!(todos.len(), 2);
    }
}
