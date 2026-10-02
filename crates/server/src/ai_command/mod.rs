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
use std::time::Duration;

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

/// Backstop detection hook, called by `flush_room` with the just-snapshotted
/// body (after the ~5s debounce). Fires on *any* `#AI!` marker — this covers
/// the case where the note was closed/flushed before the live Enter-trigger
/// fired. Cheap: a marker scan + the in-flight guard. Never blocks the caller.
pub fn maybe_spawn(room: &Arc<Room>, state: &AppState, body: &str) {
    if ops::locate_command(body).is_none() {
        return;
    }
    spawn_run(room, state);
}

/// Live detection hook, called from the inbound-edit path (`collab/ws.rs::
/// process_frame`) on every applied edit. Fires the instant the user *confirms*
/// a command by pressing Enter (the `#AI!` line becomes newline-terminated), so
/// the run starts promptly instead of waiting for the 5s flush (RC1). Being
/// server-side it works for every client, including the Android app. The shared
/// [`INFLIGHT`] guard means this and the flush backstop can't double-fire.
pub fn maybe_spawn_live(room: &Arc<Room>, state: &AppState, body: &str) {
    if !ops::has_confirmed_command(body) {
        return;
    }
    spawn_run(room, state);
}

/// Claim the in-flight slot and spawn the orchestration, or do nothing if a run
/// is already in flight for this note. All real work is on the spawned task —
/// the caller never blocks.
fn spawn_run(room: &Arc<Room>, state: &AppState) {
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

    // Lift the command line first; bail if it raced away (already stripped).
    let Some(command) = ops::locate_command(&room.snapshot_text()) else {
        state.rooms.release(room, state.clone());
        return;
    };
    let instruction = command.instruction.clone();

    // Publish the "AI" presence the INSTANT the run starts — before the strip,
    // before the (seconds-long) LLM call — and hold it for the whole run so the
    // header "AI editing…" chip lights up immediately and stays lit through
    // generation + apply (RC2/RC4). Retracted on drop at every exit path below.
    let mut presence = apply::AiPresence::begin(&room);

    // Strip the marker so it can't re-fire on this note.
    crate::collab::write::apply_text_edit(&room, ops::strip_command_line);

    // Everything from here reports failures as an appended notice + log, then
    // bails cleanly (the marker is already stripped).
    let outcome = generate_and_apply(&state, &room, &mut presence, &owner, &instruction).await;
    if let Err(reason) = outcome {
        tracing::warn!(note_id = %note_id, reason = %reason, "ai_command run failed");
        append_notice(&room, &reason);
    }

    drop(presence); // retract the "AI" presence before detaching from the room
    state.rooms.release(room, state.clone());
}

/// Idle timeout for the streaming read: if no content arrives for this long the
/// stream is considered stalled. A whole-response deadline is deliberately NOT
/// used so a long-but-progressing generation isn't cut off mid-stream.
const STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(30);

/// The fallible middle of a run: cap, resolve credentials, and generate+apply
/// edits — streaming ops in and applying each as it completes (Phase 2), with a
/// one-shot fallback if streaming can't start or yields nothing usable.
/// `Err(reason)` is a short, user-safe message for the
/// `> ⚠️ #AI! couldn't run: …` notice.
async fn generate_and_apply(
    state: &AppState,
    room: &Arc<Room>,
    presence: &mut apply::AiPresence,
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

    let endpoint = &settings.ai_endpoint;
    let model = &settings.ai_command_model;

    // Primary path: stream ops in and apply each as it completes. Fall back to
    // the one-shot call if streaming can't start OR streamed nothing usable
    // (the fallback never double-applies: it only runs when 0 ops were applied).
    let applied = match stream_and_apply(room, presence, endpoint, model, &key, &body, instruction)
        .await
    {
        Ok(n) if n > 0 => n,
        Ok(_) => {
            tracing::info!(note_id = %room.note_id, "ai_command: stream yielded no ops; one-shot fallback");
            oneshot_apply(room, presence, endpoint, model, &key, &body, instruction).await?
        }
        Err(reason) => {
            tracing::warn!(note_id = %room.note_id, reason = %reason, "ai_command: streaming unavailable; one-shot fallback");
            oneshot_apply(room, presence, endpoint, model, &key, &body, instruction).await?
        }
    };

    if applied == 0 {
        return Err("the model returned no edits".to_string());
    }
    tracing::info!(note_id = %room.note_id, applied, "ai_command applied edits");
    Ok(())
}

/// Stream the model's JSONL ops and apply each the moment its line completes.
///
/// Returns `Ok(n)` with the number of applied ops when the stream ran (even
/// `Ok(0)`), or `Err(reason)` only when the stream could not start or failed
/// **before any op was applied** (so the caller can safely one-shot-fallback
/// without double-applying). A mid-stream failure after ≥1 applied op stops and
/// returns the partial count.
///
/// Mutex invariant: the awareness `std::sync::Mutex` is touched only inside the
/// synchronous `apply::apply_op` splices/broadcasts; every `.await` here (the
/// stream read, the per-chunk sleep inside `apply_op`) happens with no guard
/// held — `rx.recv()` and the reqwest byte stream never touch the mutex.
async fn stream_and_apply(
    room: &Arc<Room>,
    presence: &mut apply::AiPresence,
    endpoint: &str,
    model: &str,
    key: &str,
    body: &str,
    instruction: &str,
) -> Result<usize, String> {
    let mut rx = llm::call_model_streaming(endpoint, model, key, body, instruction).await?;

    let mut buf = String::new();
    let mut applied = 0usize;
    loop {
        let item = match tokio::time::timeout(STREAM_IDLE_TIMEOUT, rx.recv()).await {
            Ok(Some(item)) => item,
            Ok(None) => break, // stream complete
            Err(_) => {
                // Idle timeout. If we've applied nothing, let the caller fall
                // back; otherwise keep the partial result.
                if applied > 0 {
                    tracing::warn!(note_id = %room.note_id, "ai_command: stream stalled after partial apply");
                    return Ok(applied);
                }
                return Err("the AI endpoint stalled".to_string());
            }
        };
        let delta = match item {
            Ok(d) => d,
            Err(reason) => {
                if applied > 0 {
                    tracing::warn!(note_id = %room.note_id, reason = %reason, "ai_command: stream errored after partial apply");
                    return Ok(applied);
                }
                return Err(reason);
            }
        };

        buf.push_str(&delta);
        // Apply each op the instant its line is complete.
        while let Some(nl) = buf.find('\n') {
            let line: String = buf.drain(..=nl).collect();
            if applied >= MAX_OPS {
                continue; // cap reached: drain remaining lines without applying
            }
            if let Some(op) = ops::parse_op_line(&line) {
                if apply::apply_op(room, presence, &op).await {
                    applied += 1;
                }
            }
        }
    }

    // A trailing op with no terminating newline (model stopped without one).
    if applied < MAX_OPS {
        if let Some(op) = ops::parse_op_line(&buf) {
            if apply::apply_op(room, presence, &op).await {
                applied += 1;
            }
        }
    }

    Ok(applied)
}

/// Non-streaming fallback: one-shot the model, parse ops (tolerant of either
/// JSONL or the legacy envelope), and apply them animated. Returns the number
/// of applied ops.
async fn oneshot_apply(
    room: &Arc<Room>,
    presence: &mut apply::AiPresence,
    endpoint: &str,
    model: &str,
    key: &str,
    body: &str,
    instruction: &str,
) -> Result<usize, String> {
    let content = llm::call_model(endpoint, model, key, body, instruction).await?;
    let mut parsed = ops::parse_ops_any(&content);
    parsed.truncate(MAX_OPS);
    Ok(apply::apply_all(room, presence, &parsed).await)
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
