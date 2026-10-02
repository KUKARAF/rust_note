//! The animated apply engine: play the model's ops into a live room as if a
//! collaborator named "AI" were typing.
//!
//! Each op is validated against the room's *current* text, then typed out in
//! small grapheme-sized chunks via repeated minimal `collab/write.rs::
//! apply_text_edit` splices, with the "AI" caret advancing between chunks and a
//! short pause after each so connected editors watch the text appear live. The
//! awareness `std::sync::Mutex` is never held across an `.await`:
//! `apply_text_edit` and every awareness broadcast are fully synchronous, and
//! the per-chunk `sleep` happens with no guard held.
//!
//! The "AI" presence (which lights the header "AI editing…" chip and draws the
//! in-editor caret) is owned by an [`AiPresence`] RAII handle created at the
//! very start of a run and retracted on drop — so it is up for the whole run,
//! including the generation wait, not just the apply.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Bytes;
use yrs::sync::awareness::{AwarenessUpdate, AwarenessUpdateEntry};
use yrs::sync::Message;
use yrs::updates::encoder::Encode;
use yrs::{Assoc, ClientID, IndexedSequence, Transact};

use super::ops::Op;
use crate::collab::room::{Room, CONTENT_FIELD};
use crate::collab::write::apply_text_edit;

/// Synthetic awareness client id for the "AI" collaborator. A fixed sentinel
/// (not a real `yrs` client id, which are random `u64`s), so its presence entry
/// never collides with a human peer's in practice.
const AI_CLIENT_ID: ClientID = ClientID::new(0x00A1_C0DE); // recognizable sentinel

/// Cursor color for the "AI" collaborator (a distinct, warm orange).
const AI_COLOR: &str = "#d9480f";

/// Pause between typed chunks so editors watch the text appear live.
const CHUNK_PACE: Duration = Duration::from_millis(28);

/// Upper bound on typed chunks per op, so a whole-note rewrite still animates
/// in bounded time: a longer insertion uses proportionally larger chunks
/// rather than more ticks (short inserts type 1-few graphemes at a time).
const MAX_TICKS_PER_OP: usize = 48;

/// NULL marker string yrs uses to represent a removed awareness state.
const AWARENESS_NULL: &str = "null";

/// RAII handle for the "AI" awareness presence. [`AiPresence::begin`] publishes
/// the live presence (name "AI" + color → header chip) the instant a run
/// starts; [`AiPresence::move_cursor`] moves the caret to each edit site as it
/// types; the presence is retracted on [`Drop`], so every exit path of a run —
/// including an early error return — clears it and leaves no ghost cursor.
pub struct AiPresence {
    room: Arc<Room>,
    /// Monotonic awareness clock; yrs ignores a state with a stale clock, so it
    /// must strictly increase across every update (seeded above prior residue).
    clock: u32,
}

impl AiPresence {
    /// Publish the initial "AI thinking" presence (no caret yet) and return the
    /// handle that holds it for the rest of the run.
    pub fn begin(room: &Arc<Room>) -> Self {
        let seed = {
            let awareness = room.lock_awareness();
            awareness.meta(AI_CLIENT_ID).map(|(c, _)| c).unwrap_or(0)
        };
        let clock = seed + 1;
        broadcast_ai_state(room, thinking_json(), clock);
        Self {
            room: room.clone(),
            clock,
        }
    }

    /// Move the "AI" caret to `offset` (a byte offset into the current text),
    /// encoding a real Yjs cursor so y-codemirror renders a labeled caret.
    pub fn move_cursor(&mut self, offset: usize) {
        self.clock += 1;
        let json = cursor_presence_json(&self.room, offset);
        broadcast_ai_state(&self.room, json, self.clock);
    }
}

impl Drop for AiPresence {
    fn drop(&mut self) {
        // Retract so no ghost "AI" cursor/chip lingers after the run.
        self.clock += 1;
        broadcast_ai_state(&self.room, AWARENESS_NULL.to_string(), self.clock);
    }
}

/// Apply a list of `ops` in order through `presence`, animated. Returns how
/// many ops were actually applied (ambiguous / stale anchors are skipped).
pub async fn apply_all(room: &Arc<Room>, presence: &mut AiPresence, ops: &[Op]) -> usize {
    let mut applied = 0usize;
    for op in ops {
        if apply_op(room, presence, op).await {
            applied += 1;
        }
    }
    applied
}

/// Apply a single op, typed out grapheme-by-grapheme as the "AI" caret. Returns
/// `false` (skipped, logged) if the op's anchor is absent/ambiguous in the
/// room's *current* text — re-validated here so a stale op from a stream is
/// dropped safely rather than corrupting the note.
pub async fn apply_op(room: &Arc<Room>, presence: &mut AiPresence, op: &Op) -> bool {
    let current = room.snapshot_text();
    let Some(offset) = op.validate_and_offset(&current) else {
        tracing::info!(
            note_id = %room.note_id,
            "ai_command: skipping op with absent/ambiguous anchor"
        );
        return false;
    };

    // Move the caret to the edit site before touching the text.
    presence.move_cursor(offset);

    match op {
        Op::Replace { find, with } => {
            // Delete the old span first (one splice, re-validating uniqueness),
            // then type the replacement in at that same position.
            let find = find.clone();
            if !apply_text_edit(room, move |old| super::ops::remove_unique(old, &find)) {
                return false; // raced away / became ambiguous
            }
            type_at(room, presence, offset, with).await;
        }
        Op::InsertAfter { text, .. } => type_at(room, presence, offset, text).await,
        Op::Append { text } => type_at(room, presence, offset, text).await,
    }
    true
}

/// Apply `ops` in order to `room` as the "AI" cursor, managing the presence for
/// the duration. Retained for the self-contained apply path and tests; the live
/// run manages its own [`AiPresence`] across the whole run (see `mod.rs`).
#[cfg(test)]
pub async fn apply_ops(room: &Arc<Room>, ops: &[Op]) -> usize {
    let mut presence = AiPresence::begin(room);
    apply_all(room, &mut presence, ops).await
    // `presence` drops here → retract.
}

/// Type `text` into the room starting at byte `start`, in grapheme-sized
/// chunks, advancing the "AI" caret after each chunk. Each tick is a sync
/// `apply_text_edit` splice followed by a `sleep` taken with no lock held. Stops
/// early (without panicking) if the insertion point is no longer a valid char
/// boundary — e.g. a concurrent human edit shifted the text under us.
async fn type_at(room: &Arc<Room>, presence: &mut AiPresence, start: usize, text: &str) {
    let mut pos = start;
    for piece in grapheme_chunks(text) {
        let at = pos;
        let piece_len = piece.len();
        let applied = apply_text_edit(room, move |old| {
            let before = old.get(..at)?;
            let after = old.get(at..)?;
            Some(format!("{before}{piece}{after}"))
        });
        if !applied {
            break;
        }
        pos += piece_len;
        presence.move_cursor(pos);
        tokio::time::sleep(CHUNK_PACE).await;
    }
}

/// Split `text` into char-aligned chunks for typing. Chunk size scales with
/// length so a short insertion types a grapheme or few at a time while a long
/// one still finishes within [`MAX_TICKS_PER_OP`] ticks. Char-boundary safe (no
/// byte slicing), so it never splits a multi-byte grapheme.
fn grapheme_chunks(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    if chars.is_empty() {
        return Vec::new();
    }
    let per = chars.len().div_ceil(MAX_TICKS_PER_OP).max(1);
    chars.chunks(per).map(|c| c.iter().collect()).collect()
}

/// The "AI thinking" presence (live, no caret) — drives the header chip the
/// instant a run starts, before any edit site is known.
fn thinking_json() -> String {
    serde_json::json!({
        "user": { "name": "AI", "color": AI_COLOR },
    })
    .to_string()
}

/// The "AI" presence state with a caret at byte `offset`, as a JSON string.
///
/// Carries `user` (name "AI" + color) so y-codemirror renders the collaborator,
/// a real Yjs `cursor: { anchor, head }` of encoded `StickyIndex` relative
/// positions so it draws a labeled caret that moves as the AI types, and
/// `aiCursorOffset` as a plain byte-offset hint for any non-y-codemirror
/// consumer. The caret field is omitted if a relative position can't be encoded
/// for the offset (e.g. offset 0 / empty doc), leaving presence + chip intact.
fn cursor_presence_json(room: &Arc<Room>, offset: usize) -> String {
    let mut map = serde_json::Map::new();
    map.insert(
        "user".to_string(),
        serde_json::json!({ "name": "AI", "color": AI_COLOR }),
    );
    map.insert("aiCursorOffset".to_string(), serde_json::json!(offset));
    if let Some(cursor) = encode_cursor(room, offset) {
        map.insert(
            "cursor".to_string(),
            serde_json::json!({ "anchor": cursor, "head": cursor }),
        );
    }
    serde_json::Value::Object(map).to_string()
}

/// Encode a Yjs-compatible relative position (`StickyIndex`) for `offset` in
/// the room's content text. yrs' `StickyIndex` serializes to exactly the JSON
/// shape y-codemirror feeds to `Y.createAbsolutePositionFromRelativePosition`
/// (`{"item":{"client","clock"},"assoc":0}`, or `{"tname":"content","assoc":0}`
/// at the start), so this needs no frontend change. Locks the awareness mutex
/// only for the brief, synchronous read (never across an `.await`).
fn encode_cursor(room: &Arc<Room>, offset: usize) -> Option<serde_json::Value> {
    let awareness = room.lock_awareness();
    let doc = awareness.doc();
    let text = doc.get_or_insert_text(CONTENT_FIELD);
    let txn = doc.transact();
    let index = u32::try_from(offset).ok()?;
    let sticky = text.sticky_index(&txn, index, Assoc::After)?;
    serde_json::to_value(&sticky).ok()
}

/// Apply an "AI"-client awareness entry to the room and broadcast it. A `json`
/// of `"null"` retracts the entry. Locks the awareness mutex only for the brief
/// synchronous apply (never across an `.await`).
fn broadcast_ai_state(room: &Arc<Room>, json: String, clock: u32) {
    let update = AwarenessUpdate {
        clients: HashMap::from([(
            AI_CLIENT_ID,
            AwarenessUpdateEntry {
                clock,
                json: json.into(),
            },
        )]),
    };
    {
        let awareness = room.lock_awareness();
        // Applying a foreign client id records it as a remote peer's presence;
        // a poisoned-mutex panic here matches `lock_awareness`'s policy.
        let _ = awareness.apply_update(update.clone());
    }
    let frame = Message::Awareness(update).encode_v1();
    // Origin 0: no real connection id, so every socket receives it.
    room.broadcast_frame(0, Bytes::from(frame));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize};
    use std::sync::Mutex;
    use tokio::sync::{broadcast, mpsc};
    use yrs::sync::Awareness;
    use yrs::{Doc, Text, Transact};

    fn room_with_text(text: &str) -> Arc<Room> {
        let doc = Doc::new();
        {
            let t = doc.get_or_insert_text(crate::collab::room::CONTENT_FIELD);
            let mut txn = doc.transact_mut();
            t.insert(&mut txn, 0, text);
        }
        let (broadcast_tx, _rx) = broadcast::channel(64);
        let (dirty_tx, _dirty_rx) = mpsc::unbounded_channel();
        Arc::new(Room {
            note_id: "sample".to_string(),
            awareness: Mutex::new(Awareness::new(doc)),
            broadcast: broadcast_tx,
            connections: AtomicUsize::new(1),
            dirty: AtomicBool::new(false),
            dirty_tx,
            owner_hint: "admin".to_string(),
            next_conn_id: AtomicU64::new(1),
        })
    }

    #[tokio::test]
    async fn apply_engine_edits_sample_doc_and_skips_ambiguous() {
        let room = room_with_text("Hello world.\nThis is teh note.\n");
        let mut rx = room.subscribe();

        let ops = vec![
            Op::Replace {
                find: "teh".into(),
                with: "the".into(),
            },
            Op::InsertAfter {
                find: "world.".into(),
                text: " Goodbye.".into(),
            },
            Op::Append {
                text: "\nFooter".into(),
            },
            // "o" occurs many times -> ambiguous -> skipped.
            Op::Replace {
                find: "o".into(),
                with: "0".into(),
            },
        ];

        let applied = apply_ops(&room, &ops).await;
        assert_eq!(applied, 3, "three valid ops apply, the ambiguous one skips");
        assert_eq!(
            room.snapshot_text(),
            "Hello world. Goodbye.\nThis is the note.\n\nFooter"
        );
        // Each applied splice marks the room dirty for the debounced flush.
        assert!(room.dirty.load(std::sync::atomic::Ordering::Acquire));

        // Frames were broadcast (awareness + sync updates) for live rendering.
        assert!(
            rx.try_recv().is_ok(),
            "edits + cursor must be broadcast to the room"
        );
    }

    #[tokio::test]
    async fn apply_op_types_incrementally_and_publishes_a_caret() {
        // Simulates the streaming path: ops fed one at a time through a
        // long-lived presence, each typed out with a real moving caret.
        let room = room_with_text("Hello world.\n");
        let mut presence = AiPresence::begin(&room);

        let applied = apply_op(
            &room,
            &mut presence,
            &Op::InsertAfter {
                find: "world.".into(),
                text: " Goodbye.".into(),
            },
        )
        .await;
        assert!(applied, "a valid op applies");
        assert_eq!(room.snapshot_text(), "Hello world. Goodbye.\n");

        // While the presence is live it carries a real Yjs caret (cursor:
        // {anchor, head}) plus the "AI" user — what y-codemirror renders.
        let state: serde_json::Value = {
            let awareness = room.lock_awareness();
            awareness.state(AI_CLIENT_ID).expect("AI presence is live")
        };
        assert_eq!(state["user"]["name"], "AI");
        assert!(
            state["cursor"]["anchor"]["assoc"].is_number(),
            "a StickyIndex-encoded caret is published: {state}"
        );
        assert!(state["cursor"]["head"].is_object());

        // A second op whose anchor is ambiguous is skipped, not applied.
        let skipped = apply_op(
            &room,
            &mut presence,
            &Op::Replace {
                find: "o".into(),
                with: "0".into(),
            },
        )
        .await;
        assert!(!skipped, "ambiguous anchor is skipped");
        assert_eq!(room.snapshot_text(), "Hello world. Goodbye.\n");

        drop(presence);
        let cleared: Option<serde_json::Value> = {
            let awareness = room.lock_awareness();
            awareness.state(AI_CLIENT_ID)
        };
        assert!(cleared.is_none(), "presence retracts on drop");
    }

    #[tokio::test]
    async fn apply_engine_clears_ai_presence_at_end() {
        let room = room_with_text("one two three\n");
        let ops = vec![Op::Append {
            text: "\nmore".into(),
        }];
        let _ = apply_ops(&room, &ops).await;

        // After the run the "AI" awareness state is retracted (data == None).
        let state: Option<serde_json::Value> = {
            let awareness = room.lock_awareness();
            awareness.state(AI_CLIENT_ID)
        };
        assert!(state.is_none(), "AI presence must be cleared at the end");
    }
}
