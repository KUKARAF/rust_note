//! The paced apply engine: play the model's ops into a live room one at a
//! time, as if a collaborator named "AI" were typing.
//!
//! Each op is validated against the room's *current* text, the "AI" awareness
//! cursor is moved to the edit site, the edit is spliced in via the same
//! minimal-splice + broadcast path as `collab/write.rs::apply_text_edit`, and a
//! short pause follows so connected editors see it land live. The awareness
//! mutex is never held across an `.await` (the pause happens with the guard
//! dropped).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Bytes;
use yrs::sync::awareness::{AwarenessUpdate, AwarenessUpdateEntry};
use yrs::sync::Message;
use yrs::updates::encoder::Encode;
use yrs::ClientID;

use super::ops::Op;
use crate::collab::room::Room;
use crate::collab::write::apply_text_edit;

/// Synthetic awareness client id for the "AI" collaborator. A fixed sentinel
/// (not a real `yrs` client id, which are random `u64`s), so its presence entry
/// never collides with a human peer's in practice.
const AI_CLIENT_ID: ClientID = ClientID::new(0x00A1_C0DE); // recognizable sentinel

/// Cursor color for the "AI" collaborator (a distinct, warm orange).
const AI_COLOR: &str = "#d9480f";

/// Pause between ops so editors watch the note get edited live.
const PACE: Duration = Duration::from_millis(70);

/// NULL marker string yrs uses to represent a removed awareness state.
const AWARENESS_NULL: &str = "null";

/// Apply `ops` in order to `room`, paced, as the "AI" cursor. Returns how many
/// ops were actually applied (ambiguous / stale anchors are skipped + logged).
///
/// Publishes the "AI" presence entry while editing and clears it at the end.
pub async fn apply_ops(room: &Arc<Room>, ops: &[Op]) -> usize {
    // Seed the awareness clock above any residue from a previous run so our
    // updates are always accepted (yrs ignores a state whose clock is stale).
    let mut clock = {
        let awareness = room.lock_awareness();
        awareness.meta(AI_CLIENT_ID).map(|(c, _)| c).unwrap_or(0)
    };

    let mut applied = 0usize;
    for op in ops {
        let current = room.snapshot_text();
        let Some(offset) = op.validate_and_offset(&current) else {
            tracing::info!(
                note_id = %room.note_id,
                "ai_command: skipping op with absent/ambiguous anchor"
            );
            continue;
        };

        // Move the "AI" cursor to this edit before splicing it in.
        clock += 1;
        broadcast_ai_state(room, presence_json(offset), clock);

        if apply_text_edit(room, |old| op.apply_to(old)) {
            applied += 1;
            tokio::time::sleep(PACE).await;
        }
    }

    // Retract the "AI" presence so no ghost cursor lingers.
    clock += 1;
    broadcast_ai_state(room, AWARENESS_NULL.to_string(), clock);

    applied
}

/// The "AI" presence state as a JSON string.
///
/// The MUST-have is live presence (name "AI" + a distinct color) so
/// `y-codemirror` renders the "AI" collaborator. `aiCursorOffset` carries the
/// byte offset of the current edit as a hint.
///
/// TODO(ai-command): render a precise remote *caret*. y-codemirror positions
/// cursors from a `cursor: { anchor, head }` pair of `Y.RelativePosition`
/// JSON, not a byte offset; encoding that from the server is the deferred
/// "fiddly" part called out in the contract. v1 ships presence only.
fn presence_json(offset: usize) -> String {
    serde_json::json!({
        "user": { "name": "AI", "color": AI_COLOR },
        "aiCursorOffset": offset,
    })
    .to_string()
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
