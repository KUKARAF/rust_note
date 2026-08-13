//! Aggregated task ("todo") views across daily notes.
//!
//! Daily notes (`diary/YYYY-MM-DD`) hold the bulk of the vault's checkbox
//! tasks; this module walks them, parses each with [`rust_note_core::tasks`],
//! and serves the flattened set so the web `/todo` board can group/sort them
//! without fetching every note's body itself.

pub mod query;
pub mod routes;
