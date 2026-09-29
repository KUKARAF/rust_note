//! Write-through-room note edits.
//!
//! Any server-side mutation of a note's content (stats logging, MCP tools, …)
//! must go through the note's live collab [`Room`] rather than writing the file
//! directly: a note open in the editor keeps the authoritative in-memory CRDT
//! doc and re-flushes it a few seconds later, so a bare file write would be
//! clobbered. Applying the edit as a minimal CRDT splice on the same room both
//! survives that flush and shows up live in any connected editor.
//!
//! [`edit_note_through_room`] is the reusable entry point; it does NOT perform
//! ACL — the caller is responsible for registering/authorizing the note first.

use std::sync::Arc;

use axum::body::Bytes;
use yrs::sync::{Message, SyncMessage};
use yrs::updates::encoder::Encode;
use yrs::{GetString, ReadTxn, Text, Transact};

use super::room::{Room, CONTENT_FIELD};
use crate::error::AppResult;
use crate::state::AppState;

/// Apply a whole-text edit to a room's CRDT doc as a minimal splice, broadcast
/// it to connected sockets, and mark the room dirty.
///
/// `edit_fn(old_text) -> Option<new_text>`: return `None` to refuse the edit
/// (e.g. the target couldn't be located). Returns whether the room now reflects
/// the intended state — `true` when an edit was applied *or* the text already
/// matched, `false` only when `edit_fn` refused.
pub(crate) fn apply_text_edit(
    room: &Arc<Room>,
    edit_fn: impl FnOnce(&str) -> Option<String>,
) -> bool {
    let awareness = room.lock_awareness();
    let doc = awareness.doc();
    let text = doc.get_or_insert_text(CONTENT_FIELD);
    let old = text.get_string(&doc.transact());
    let Some(new) = edit_fn(&old) else {
        return false;
    };
    if new == old {
        return true;
    }
    let before_sv = doc.transact().state_vector();
    {
        let mut txn = doc.transact_mut();
        let (p, s) = byte_diff(&old, &new);
        let del_len = old.len() - s - p;
        if del_len > 0 {
            text.remove_range(&mut txn, p as u32, del_len as u32);
        }
        let ins = &new[p..new.len() - s];
        if !ins.is_empty() {
            text.insert(&mut txn, p as u32, ins);
        }
    }
    let update = doc.transact().encode_state_as_update_v1(&before_sv);
    drop(awareness);

    let frame = Message::Sync(SyncMessage::Update(update)).encode_v1();
    room.broadcast_frame(0, Bytes::from(frame)); // origin 0: no real conn uses it
    room.mark_dirty();
    true
}

/// Longest common (prefix, suffix) byte lengths between `old` and `new`, snapped
/// to char boundaries. The differing middle is `old[p..len-s]` → `new[p..len-s]`.
/// Offsets are UTF-8 bytes to match the server doc's `OffsetKind::Bytes`.
pub(crate) fn byte_diff(old: &str, new: &str) -> (usize, usize) {
    let (ob, nb) = (old.as_bytes(), new.as_bytes());
    let max_p = ob.len().min(nb.len());
    let mut p = 0;
    while p < max_p && ob.get(p) == nb.get(p) {
        p += 1;
    }
    while p > 0 && !old.is_char_boundary(p) {
        p -= 1;
    }
    let max_s = (ob.len() - p).min(nb.len() - p);
    let mut s = 0;
    while s < max_s && ob.get(ob.len() - 1 - s) == nb.get(nb.len() - 1 - s) {
        s += 1;
    }
    while s > 0 && !old.is_char_boundary(ob.len() - s) {
        s -= 1;
    }
    (p, s)
}

/// Edit a note's content through its live collab room and flush synchronously to
/// git, so the change is durable and visible to a `read_file`/list immediately.
///
/// The caller MUST have registered + authorized the note first (this does no
/// ACL). `owner_hint` seeds ownership if the room has to create a brand-new note
/// (used for e.g. a not-yet-existing daily note). Returns whether the edit was
/// applied (see [`apply_text_edit`]).
pub(crate) async fn edit_note_through_room(
    state: &AppState,
    note_id: &str,
    owner_hint: &str,
    edit_fn: impl FnOnce(&str) -> Option<String>,
) -> AppResult<bool> {
    let room = state.rooms.get_or_create(note_id, owner_hint, state).await;
    let applied = apply_text_edit(&room, edit_fn);
    if applied {
        if let Err(err) = super::persist::flush_room(&room, state).await {
            tracing::error!(note_id = %note_id, error = %err, "write-through flush failed");
        }
    }
    state.rooms.release(room, state.clone());
    Ok(applied)
}
