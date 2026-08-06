//! Per-user settings, stored as YAML frontmatter in a git-backed markdown
//! note (not SQLite) — see `store::settings_note_id`.

pub mod routes;
pub mod store;
