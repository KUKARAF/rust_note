//! `rust-note-core`: shared, platform-independent domain logic for rust_note.
//!
//! This crate is intended to be used by both the `server` crate and the
//! `mobile` (Tauri) crate, so it must stay free of server-only or
//! mobile-only dependencies (no `axum`, no `tauri`).

pub mod device_token;
pub mod frontmatter;
pub mod note_id;
pub mod share_token;
pub mod tasks;
pub mod vimwiki;
