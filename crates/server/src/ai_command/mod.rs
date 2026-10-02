//! In-note `#AI!` editing (see `docs/ai-command.md`).
//!
//! A line whose trimmed text ends with `#AI!` is an **instruction**. On the
//! next debounced flush the server lifts it out, strips the marker, asks an LLM
//! for a small set of anchored find/replace ops, and plays them back into the
//! note's live Yjs doc as an "AI" cursor — so every connected editor watches
//! the note get edited in real time.
//!
//! Wiring: [`maybe_spawn`] is the detection hook called from
//! `collab/persist.rs::flush_room`. It scans the just-flushed body, and if it
//! finds the marker (and no run is already in flight for the note) it spawns
//! [`run`] and returns immediately — the flush is never blocked on an LLM call.
//! [`run`] strips the marker first (so it can't re-fire), calls the model, and
//! drives [`apply::apply_ops`]. Loop-safety is the combination of the strip and
//! the per-note in-flight guard.

mod apply;
mod llm;
mod ops;

use std::sync::Arc;
use std::sync::LazyLock;

use dashmap::mapref::entry::Entry;
use dashmap::DashMap;

use crate::collab::room::Room;
use crate::settings::store;
use crate::state::AppState;

/// Notes whose `#AI!` run is currently in flight, so a later flush of the same
/// note (before the marker strip has propagated) doesn't start a second run.
/// Process-global (like the room registry) rather than on `AppState`, so adding
/// it doesn't ripple through every `AppState` constructor.
static INFLIGHT: LazyLock<DashMap<String, ()>> = LazyLock::new(DashMap::new);

/// Skip notes larger than this (post-strip) — a huge note is both costly to
/// send and unlikely to be an intentional inline-edit target.
const MAX_NOTE_BYTES: usize = 20 * 1024;

/// Hard cap on the number of ops applied from one model response.
const MAX_OPS: usize = 40;

/// RAII in-flight guard: present in [`INFLIGHT`] for its lifetime.
struct InflightGuard {
    note_id: String,
}

impl InflightGuard {
    /// Claim the in-flight slot for `note_id`, or `None` if a run is already
    /// in flight for it.
    fn try_acquire(note_id: &str) -> Option<Self> {
        match INFLIGHT.entry(note_id.to_string()) {
            Entry::Occupied(_) => None,
            Entry::Vacant(slot) => {
                slot.insert(());
                Some(Self {
                    note_id: note_id.to_string(),
                })
            }
        }
    }
}

impl Drop for InflightGuard {
    fn drop(&mut self) {
        INFLIGHT.remove(&self.note_id);
    }
}

/// Detection hook, called by `flush_room` with the just-snapshotted body.
///
/// Cheaply scans for the `#AI!` marker; if present and no run is in flight for
/// this note, claims the in-flight guard and spawns the orchestration. Never
/// blocks (the caller holds the per-note lock) — all real work is on the task.
pub fn maybe_spawn(room: &Arc<Room>, state: &AppState, body: &str) {
    if ops::locate_command(body).is_none() {
        return;
    }
    let Some(guard) = InflightGuard::try_acquire(&room.note_id) else {
        return; // already running for this note
    };

    let note_id = room.note_id.clone();
    let fallback_owner = room.owner_hint.clone();
    let state = state.clone();
    tokio::spawn(async move {
        // Hold the guard for the whole run; released on drop.
        let _guard = guard;
        run(state, note_id, fallback_owner).await;
    });
}

/// Orchestrate one `#AI!` run against the note's live room.
async fn run(state: AppState, note_id: String, fallback_owner: String) {
    // Resolve the note's owner for settings/key lookup (fall back to the
    // room's owner hint if the row is somehow missing).
    let owner = note_owner(&state, &note_id)
        .await
        .unwrap_or(fallback_owner.clone());

    // Attach to the room ONCE (bumps the connection count so it isn't reaped
    // mid-run); release at the end.
    let room = state
        .rooms
        .get_or_create(&note_id, &fallback_owner, &state)
        .await;

    // Lift + strip the command line first, so the marker is gone before
    // anything else can re-fire on it.
    let Some(command) = ops::locate_command(&room.snapshot_text()) else {
        // Raced away (already stripped); nothing to do.
        state.rooms.release(room, state.clone());
        return;
    };
    let instruction = command.instruction.clone();
    crate::collab::write::apply_text_edit(&room, ops::strip_command_line);

    // Everything from here reports failures as an appended notice + log, then
    // bails cleanly (the marker is already stripped).
    let outcome = generate_and_apply(&state, &room, &owner, &instruction).await;
    if let Err(reason) = outcome {
        tracing::warn!(note_id = %note_id, reason = %reason, "ai_command run failed");
        append_notice(&room, &reason);
    }

    state.rooms.release(room, state.clone());
}

/// The fallible middle of a run: cap, resolve credentials, call the model,
/// parse ops, and apply them. `Err(reason)` is a short, user-safe message for
/// the `> ⚠️ #AI! couldn't run: …` notice.
async fn generate_and_apply(
    state: &AppState,
    room: &Arc<Room>,
    owner: &str,
    instruction: &str,
) -> Result<(), String> {
    // Post-strip body is the current room text.
    let body = room.snapshot_text();
    if body.len() > MAX_NOTE_BYTES {
        return Err("note is too large".to_string());
    }

    let settings = {
        let settings_note = store::settings_note_id(owner);
        let _guard = state.note_locks.lock(&settings_note).await;
        store::load_or_bootstrap(state, owner)
            .await
            .map_err(|_| "could not load settings".to_string())?
    };

    // Key resolution mirrors `todos/query.rs`: per-user key, else the server
    // config key, else refuse.
    let key = if !settings.openrouter_api_key.is_empty() {
        settings.openrouter_api_key.clone()
    } else if let Some(k) = &state.config.openrouter_api_key {
        k.clone()
    } else {
        return Err("no AI API key configured".to_string());
    };

    let content = llm::call_model(
        &settings.ai_endpoint,
        &settings.ai_command_model,
        &key,
        &body,
        instruction,
    )
    .await?;

    let mut parsed = ops::parse_ops(&content)?;
    if parsed.is_empty() {
        return Err("the model returned no edits".to_string());
    }
    parsed.truncate(MAX_OPS);

    let applied = apply::apply_ops(room, &parsed).await;
    tracing::info!(
        note_id = %room.note_id,
        requested = parsed.len(),
        applied,
        "ai_command applied edits"
    );
    Ok(())
}

/// Look up the note's owner id from the `notes` table.
async fn note_owner(state: &AppState, note_id: &str) -> Option<String> {
    let row: Option<(String,)> = sqlx::query_as("SELECT owner_id FROM notes WHERE id = ?")
        .bind(note_id)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten();
    row.map(|(owner,)| owner)
}

/// Append a one-line failure notice to the note (loop-safe: the marker is
/// already stripped before this is called).
fn append_notice(room: &Arc<Room>, reason: &str) {
    let suffix = format!("\n> ⚠️ #AI! couldn't run: {reason}\n");
    crate::collab::write::apply_text_edit(room, |old| Some(format!("{old}{suffix}")));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inflight_guard_is_exclusive_then_releasable() {
        let first = InflightGuard::try_acquire("note-x").expect("first acquire succeeds");
        assert!(
            InflightGuard::try_acquire("note-x").is_none(),
            "a second concurrent acquire is refused"
        );
        drop(first);
        assert!(
            InflightGuard::try_acquire("note-x").is_some(),
            "slot is reusable after the guard drops"
        );
    }
}
