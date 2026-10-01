//! Pipeline tracker: leads + applications as git-backed notes under
//! `pipeline/`.
//!
//! Each item is one markdown note whose flat frontmatter is parsed by
//! [`rust_note_core::leads`]; this module walks those notes (exactly like
//! [`crate::todos`]), ACL-filters them, prefers a live collab room's text over
//! disk, and serves the aggregated set so the web `/pipeline` board can
//! group/sort without fetching each note's body itself.

pub mod routes;
