//! Server-side auto-stamping of pipeline **stage transitions**.
//!
//! When a `pipeline/` note's `stage` changes between the previously-committed
//! on-disk content and the just-flushed text, the server appends a
//! `"<new_stage>@<now_utc_rfc3339>"` entry to the note's append-only
//! `stage_history` comma-list and bumps `last_activity_at` to now. The parsed
//! shape of `stage_history` lives in [`rust_note_core::leads`].
//!
//! Wiring: [`maybe_stamp`] is the detection hook called from
//! `collab/persist.rs::flush_room` with the pre-write disk content and the
//! just-flushed text. It only *decides + spawns*; the (collab-safe) write-back
//! runs on a spawned task so the flush is never blocked — mirroring how
//! [`crate::ai_command::maybe_spawn`] is wired into the same flush.
//!
//! # Loop-safety
//!
//! The write-back touches only `stage_history` and `last_activity_at`, never
//! `stage`. So the flush it triggers sees the same `stage` on both the
//! previously-committed content and the new text, [`stage_transition`] returns
//! `None`, and no second stamp is produced. Because "previous stage" is read
//! from committed disk state (not room memory), a redundant flush of an
//! already-stamped change is also a no-op. No in-flight guard is needed; a
//! per-note guard would in fact risk dropping two legitimate back-to-back
//! transitions, so it is deliberately omitted.

use std::sync::Arc;

use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use rust_note_core::frontmatter::Frontmatter;
use rust_note_core::leads::PipelineItem;

use super::room::Room;
use crate::state::AppState;

/// Detection hook: called by `flush_room` after a successful commit with the
/// note's previously-committed on-disk content (`previous`) and the
/// just-flushed text (`current`). If this is a pipeline note whose `stage`
/// changed, spawn a collab-safe write-back that logs the transition. Never
/// blocks the caller (only a parse + a `tokio::spawn`).
pub fn maybe_stamp(room: &Arc<Room>, state: &AppState, previous: &str, current: &str) {
    let Some(new_stage) = stage_transition(&room.note_id, previous, current) else {
        return;
    };

    // Format `now` once up front; a formatting failure is practically
    // impossible for `now_utc()`, but degrade to "skip" rather than panic.
    let Ok(now) = OffsetDateTime::now_utc().format(&Rfc3339) else {
        return;
    };
    let entry = format!("{new_stage}@{now}");

    // The write-back goes through the live collab room (never a raw file
    // write) so a note open in an editor merges the change instead of being
    // clobbered — same path `#AI!` uses. Spawned so the flush isn't blocked.
    let note_id = room.note_id.clone();
    let owner_hint = room.owner_hint.clone();
    let state = state.clone();
    tokio::spawn(async move {
        let result =
            crate::collab::write::edit_note_through_room(&state, &note_id, &owner_hint, |old| {
                Some(append_stage_entry(old, &entry, &now))
            })
            .await;
        match result {
            Ok(_) => {
                tracing::info!(note_id = %note_id, stage = %new_stage, "stamped stage transition")
            }
            Err(err) => {
                tracing::error!(note_id = %note_id, error = %err, "stage-history stamp write-back failed")
            }
        }
    });
}

/// Decide whether a stage transition should be logged for `note_id`.
///
/// Returns the new stage to stamp when `current` is a pipeline item with a
/// real `stage` that differs from the `previous` content's stage (a first-time
/// set — `None`/absent → a real stage — counts). Returns `None` when `current`
/// isn't a pipeline note, has no stage, or the stage is unchanged.
fn stage_transition(note_id: &str, previous: &str, current: &str) -> Option<String> {
    let new_stage = pipeline_stage(note_id, current)?;
    if pipeline_stage(note_id, previous).as_deref() == Some(new_stage.as_str()) {
        return None;
    }
    Some(new_stage)
}

/// Parse `content` as a pipeline item and return its trimmed, non-empty
/// `stage`. `None` when the note isn't a pipeline item or carries no stage.
fn pipeline_stage(note_id: &str, content: &str) -> Option<String> {
    let fm = Frontmatter::parse(content);
    PipelineItem::from_frontmatter(note_id, &fm)?.stage
}

/// Append `entry` to the note's `stage_history` comma-list and set
/// `last_activity_at` to `now`, returning the re-rendered note.
///
/// Existing history (including malformed entries) is preserved verbatim and
/// simply extended. Crucially this never touches `stage`, which is what makes
/// the triggered re-flush loop-safe (see the module docs).
fn append_stage_entry(old: &str, entry: &str, now: &str) -> String {
    let mut fm = Frontmatter::parse(old);
    let history = match fm.get("stage_history").map(str::trim) {
        Some(existing) if !existing.is_empty() => format!("{existing}, {entry}"),
        _ => entry.to_string(),
    };
    fm.set("stage_history", &history);
    fm.set("last_activity_at", now);
    fm.render()
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOTE: &str = "pipeline/acme-gig";

    fn item(content: &str) -> PipelineItem {
        let fm = Frontmatter::parse(content);
        PipelineItem::from_frontmatter(NOTE, &fm).expect("pipeline item")
    }

    #[test]
    fn first_time_stage_set_is_a_transition() {
        // `None`/absent → a real stage counts as a change.
        let prev = "---\nkind: lead\ncompany: Acme\n---\n";
        let next = "---\nkind: lead\ncompany: Acme\nstage: lead\n---\n";
        assert_eq!(stage_transition(NOTE, prev, next).as_deref(), Some("lead"));
    }

    #[test]
    fn changed_stage_is_a_transition() {
        let prev = "---\nkind: lead\ncompany: Acme\nstage: lead\n---\n";
        let next = "---\nkind: lead\ncompany: Acme\nstage: interview\n---\n";
        assert_eq!(
            stage_transition(NOTE, prev, next).as_deref(),
            Some("interview")
        );
    }

    #[test]
    fn unchanged_stage_is_not_a_transition() {
        let prev = "---\nkind: lead\ncompany: Acme\nstage: interview\n---\n# body\n";
        // A second, unrelated edit (body change) that leaves `stage` the same.
        let next = "---\nkind: lead\ncompany: Acme\nstage: interview\n---\n# body edit\n";
        assert!(stage_transition(NOTE, prev, next).is_none());
    }

    #[test]
    fn non_pipeline_note_is_never_a_transition() {
        let prev = "just a diary\n";
        let next = "just a diary\nwith a stage: word in prose\n";
        assert!(stage_transition(NOTE, prev, next).is_none());
    }

    #[test]
    fn cleared_stage_is_not_logged() {
        // Removing the stage has no new stage to stamp.
        let prev = "---\nkind: lead\ncompany: Acme\nstage: interview\n---\n";
        let next = "---\nkind: lead\ncompany: Acme\n---\n";
        assert!(stage_transition(NOTE, prev, next).is_none());
    }

    #[test]
    fn append_to_empty_history_sets_one_entry_and_bumps_activity() {
        let old = "---\nkind: lead\ncompany: Acme\nstage: lead\n---\n# Acme\n";
        let rendered = append_stage_entry(old, "lead@2026-09-14T09:00:00Z", "2026-09-14T09:00:00Z");
        let it = item(&rendered);
        assert_eq!(it.stage.as_deref(), Some("lead"));
        assert_eq!(it.stage_history.len(), 1);
        assert_eq!(it.stage_history[0].stage, "lead");
        assert_eq!(it.last_activity_at.as_deref(), Some("2026-09-14T09:00:00Z"));
        // Body is untouched.
        assert!(rendered.ends_with("# Acme\n"));
    }

    #[test]
    fn append_extends_existing_history_in_order() {
        let old = "---\nkind: lead\ncompany: Acme\nstage: interview\nstage_history: \"lead@2026-09-14T09:00:00Z\"\n---\n";
        let rendered = append_stage_entry(
            old,
            "interview@2026-09-20T14:00:00Z",
            "2026-09-20T14:00:00Z",
        );
        let it = item(&rendered);
        let stages: Vec<&str> = it.stage_history.iter().map(|e| e.stage.as_str()).collect();
        assert_eq!(stages, vec!["lead", "interview"]);
    }

    #[test]
    fn append_preserves_malformed_existing_history() {
        let old = "---\nkind: lead\ncompany: Acme\nstage: offer\nstage_history: \"garbage-no-at, lead@2026-09-14T09:00:00Z\"\n---\n";
        let rendered =
            append_stage_entry(old, "offer@2026-09-28T10:00:00Z", "2026-09-28T10:00:00Z");
        // The raw malformed token is retained verbatim in the stored value...
        let fm = Frontmatter::parse(&rendered);
        assert_eq!(
            fm.get("stage_history"),
            Some("garbage-no-at, lead@2026-09-14T09:00:00Z, offer@2026-09-28T10:00:00Z")
        );
        // ...while the typed view drops only the malformed one.
        let it = item(&rendered);
        let stages: Vec<&str> = it.stage_history.iter().map(|e| e.stage.as_str()).collect();
        assert_eq!(stages, vec!["lead", "offer"]);
    }

    #[test]
    fn stamp_write_back_is_loop_safe() {
        // Simulate the committed state after a real stage change (new stage,
        // no history yet), then the write-back's output (history added, same
        // stage). A re-flush comparing those two must NOT re-stamp.
        let committed = "---\nkind: lead\ncompany: Acme\nstage: interview\n---\n";
        let after_stamp = append_stage_entry(
            committed,
            "interview@2026-09-20T14:00:00Z",
            "2026-09-20T14:00:00Z",
        );
        assert!(
            stage_transition(NOTE, committed, &after_stamp).is_none(),
            "appending history must not look like a stage change"
        );
    }
}
