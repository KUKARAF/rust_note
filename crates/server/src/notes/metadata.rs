//! Note metadata (title, owner, timestamps) assembled from a mix of the
//! `notes` sqlite table (ownership, fallback `created_at`) and the note's
//! git history (real `created_at`/`updated_at`, when available).

use serde::Serialize;

use crate::notes::fs_store::note_id_to_path;
use crate::state::AppState;

#[derive(Debug, Clone, Serialize)]
pub struct NoteMeta {
    pub id: String,
    pub title: String,
    pub owner_id: String,
    pub created_at: String,
    pub updated_at: String,
    /// Opaque optimistic-concurrency token: the sha of the note's most
    /// recent git commit (empty string if the note has no history yet,
    /// e.g. transiently between DB registration and its first commit).
    /// Clients that read a note back should echo this as `expected_version`
    /// on a subsequent `PUT` so the server can detect that someone else
    /// wrote a newer version in the meantime (see
    /// `silent-lost-update-no-conflict-detection`).
    pub version: String,
}

/// Extract a title from the first `# Heading` line of `content`, falling
/// back to the last path segment of `note_id` (with `-`/`_` turned into
/// spaces) if no such heading exists.
pub fn extract_title(note_id: &str, content: &str) -> String {
    // Excalidraw drawings (Obsidian plugin wrappers, id `*.excalidraw`) have
    // only internal headings ("# Excalidraw Data", "## Text Elements") that
    // would make a useless title - always title them from the filename.
    if let Some(stem) = note_id.strip_suffix(".excalidraw") {
        return stem
            .rsplit('/')
            .next()
            .unwrap_or(stem)
            .replace(['-', '_'], " ");
    }

    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(heading) = trimmed.strip_prefix('#') {
            let heading = heading.trim_start_matches('#').trim();
            if !heading.is_empty() {
                return heading.to_string();
            }
        }
    }

    note_id
        .rsplit('/')
        .next()
        .unwrap_or(note_id)
        .replace(['-', '_'], " ")
}

/// Assemble a [`NoteMeta`] for `note_id` from its content plus the `notes`
/// table and git history.
pub async fn build_meta(
    state: &AppState,
    note_id: &str,
    content: &str,
) -> anyhow::Result<NoteMeta> {
    let rel_path = note_id_to_path(note_id);

    let row: Option<(String, String)> =
        sqlx::query_as("SELECT owner_id, created_at FROM notes WHERE id = ?")
            .bind(note_id)
            .fetch_optional(&state.db)
            .await?;

    let (owner_id, db_created_at) = row.unwrap_or_else(|| (String::new(), String::new()));

    let history = state
        .notes_repo
        .history(&rel_path)
        .await
        .unwrap_or_default();
    // `history` is newest-first (see `NotesRepo::history`).
    let updated_at = history
        .first()
        .map(|c| c.timestamp.clone())
        .unwrap_or_else(|| db_created_at.clone());
    let created_at = history
        .last()
        .map(|c| c.timestamp.clone())
        .unwrap_or(db_created_at);
    let version = history.first().map(|c| c.sha.clone()).unwrap_or_default();

    Ok(NoteMeta {
        id: note_id.to_string(),
        title: extract_title(note_id, content),
        owner_id,
        created_at,
        updated_at,
        version,
    })
}

/// Assemble a [`NoteMeta`] for `note_id` **cheaply**, for the note-list
/// endpoint: `owner_id`/`created_at`/`updated_at` come straight from the
/// `notes` table, and the title from `content`. Crucially this does NOT walk
/// the note's git history — doing that per note under the global repo mutex is
/// what made `GET /api/notes` O(notes × commits) and a trivial DoS
/// (dos-01/dos-02).
///
/// `version` is intentionally left empty here: the list view never needs the
/// optimistic-concurrency token (only the single-note `GET`/`PUT`/`POST`
/// responses, which still populate it from git history via [`build_meta`]).
pub async fn build_list_meta(
    state: &AppState,
    note_id: &str,
    content: &str,
) -> anyhow::Result<NoteMeta> {
    let row: Option<(String, String, Option<String>)> =
        sqlx::query_as("SELECT owner_id, created_at, updated_at FROM notes WHERE id = ?")
            .bind(note_id)
            .fetch_optional(&state.db)
            .await?;

    let (owner_id, created_at, updated_at) = row
        .map(|(o, c, u)| {
            // Rows created before the `updated_at` migration (or transiently
            // before the first touch) fall back to `created_at`.
            let updated = u.unwrap_or_else(|| c.clone());
            (o, c, updated)
        })
        .unwrap_or_else(|| (String::new(), String::new(), String::new()));

    Ok(NoteMeta {
        id: note_id.to_string(),
        title: extract_title(note_id, content),
        owner_id,
        created_at,
        updated_at,
        version: String::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_title_from_heading() {
        assert_eq!(extract_title("foo", "# Hello World\nbody"), "Hello World");
        assert_eq!(extract_title("foo", "some text\n## Sub\nmore"), "Sub");
    }

    #[test]
    fn extract_title_fallback_from_id() {
        assert_eq!(
            extract_title("projects/my-note", "no heading here"),
            "my note"
        );
    }

    #[test]
    fn extract_title_excalidraw_uses_filename_not_internal_headings() {
        // The wrapper's internal headings must never become the title.
        assert_eq!(
            extract_title(
                "layer55/High level overview.excalidraw",
                "---\nexcalidraw-plugin: parsed\n---\n# Excalidraw Data\n## Text Elements\n"
            ),
            "High level overview"
        );
    }
}
