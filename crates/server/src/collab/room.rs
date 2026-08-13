//! In-memory collaboration "rooms" — one per note currently being
//! co-edited — plus the registry that owns them.
//!
//! # Concurrency model
//!
//! A [`Room`] holds a single authoritative `yrs` [`Doc`] wrapped in an
//! [`Awareness`] (presence) struct. Neither is manipulated across an
//! `.await`: the [`Awareness`] lives behind a plain [`std::sync::Mutex`]
//! that is only ever locked for the brief, synchronous encode/apply steps
//! the Yjs sync protocol needs (see [`super::ws`]). Fan-out to the other
//! connected sockets goes through a Tokio [`broadcast`] channel — that is
//! the only async part, and it never touches the mutex.
//!
//! Each broadcast payload is tagged with the *origin* connection id so a
//! socket can skip echoing a message back to the connection that produced
//! it (Yjs would treat the echo as an idempotent no-op, but skipping it
//! saves bandwidth and a redundant re-apply on the client).
//!
//! # Lifecycle
//!
//! Rooms are created lazily by [`RoomRegistry::get_or_create`], seeding the
//! doc's `"content"` text from the note's current on-disk markdown. A
//! per-room debounced persistence task (see [`super::persist`]) is spawned
//! at creation and flushes edits back to git. When the last connection
//! drops, a grace-period reaper flushes once more and removes the room from
//! the registry — unless a fresh connection arrived in the meantime.

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Bytes;
use dashmap::mapref::entry::Entry;
use dashmap::DashMap;
use tokio::sync::{broadcast, mpsc};
use yrs::sync::Awareness;
use yrs::{Doc, GetString, Text, Transact};

use crate::notes::fs_store::note_id_to_path;
use crate::state::AppState;

/// Name of the `yrs` shared text holding a note's markdown. Must match the
/// field the frontend binds its editor to (and the spike's `protocol.rs`).
pub const CONTENT_FIELD: &str = "content";

/// Capacity of each room's broadcast channel. If a slow socket falls this
/// far behind it receives a `Lagged` error and is resynced by the client's
/// own periodic sync; it does not block other sockets.
const BROADCAST_CAPACITY: usize = 512;

/// How long to keep an emptied room (and its seeded doc state) around before
/// reaping it, so a brief reconnect/refresh doesn't discard in-memory state
/// or force a re-seed from disk.
const REAP_GRACE: Duration = Duration::from_secs(10);

/// One encoded y-protocol frame fanned out to a room's sockets, tagged with
/// the connection that produced it so that connection can skip its own echo.
#[derive(Clone)]
pub struct RelayMsg {
    /// Connection id that originated this frame (see [`Room::next_conn_id`]).
    pub origin: u64,
    /// The encoded `yrs::sync::Message` bytes (cheap to clone — `Bytes`).
    pub data: Bytes,
}

/// A single live collaborative-editing session for one note.
pub struct Room {
    /// The note id this room edits (e.g. `projects/foo`).
    pub note_id: String,
    /// Authoritative CRDT doc + presence. Guarded by a *sync* mutex held
    /// only for brief, non-`await` encode/apply operations.
    pub awareness: Mutex<Awareness>,
    /// Fan-out channel for doc + awareness frames to all connected sockets.
    pub broadcast: broadcast::Sender<RelayMsg>,
    /// Number of live WebSocket connections attached to this room.
    pub connections: AtomicUsize,
    /// Set when the doc has edits not yet flushed to git; cleared by a
    /// successful flush (see [`super::persist::flush_room`]).
    pub dirty: AtomicBool,
    /// Nudges the persistence task that there is something to flush.
    pub dirty_tx: mpsc::UnboundedSender<()>,
    /// User to attribute ownership to if the note has no `notes` row yet
    /// when it is first flushed (the first user who opened the room). The
    /// normal flow only opens *existing* notes, so this is a safety net.
    pub owner_hint: String,
    /// Monotonic per-connection id source.
    pub(crate) next_conn_id: AtomicU64,
}

impl Room {
    /// Allocate the next connection id for a socket joining this room.
    pub fn next_conn_id(&self) -> u64 {
        self.next_conn_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Subscribe a new socket to this room's fan-out channel.
    pub fn subscribe(&self) -> broadcast::Receiver<RelayMsg> {
        self.broadcast.subscribe()
    }

    /// Lock the room's CRDT doc/awareness for a brief, non-`await` encode or
    /// apply operation.
    ///
    /// `std::sync::Mutex::lock()` only returns `Err` (poisoned) if some
    /// *other* task already panicked while holding this exact lock mid-
    /// mutation of the shared Yjs document - meaning the in-memory CRDT
    /// state may genuinely be inconsistent. Per the project's panic-safety
    /// policy (see `rust-advice.md`), that is exactly the kind of
    /// unrecoverable state where crashing is the right call rather than
    /// silently serving possibly-corrupted collaborative content to users:
    /// "it's far better to kill the program when an unrecoverable state has
    /// been reached than carry on with weird undefined memory behavior."
    /// Centralized here (rather than at each of the many call sites) so the
    /// panic-safety lint only needs one documented exemption.
    #[allow(clippy::expect_used)]
    pub fn lock_awareness(&self) -> std::sync::MutexGuard<'_, Awareness> {
        self.awareness.lock().expect("awareness mutex poisoned")
    }

    /// Fan a frame out to every other socket in the room. `origin` is the
    /// id of the connection that produced it, so it can skip its own echo.
    pub fn broadcast_frame(&self, origin: u64, data: Bytes) {
        // Errors only when there are zero receivers, which is fine.
        let _ = self.broadcast.send(RelayMsg { origin, data });
    }

    /// Mark the doc dirty and wake the persistence task.
    pub fn mark_dirty(&self) {
        self.dirty.store(true, Ordering::Release);
        // Unbounded send only fails if the persist task is gone (room being
        // torn down); nothing useful to do in that case.
        let _ = self.dirty_tx.send(());
    }

    /// Current full text of the note, read under the mutex. Never holds the
    /// lock across an `.await` — it returns an owned `String`.
    pub fn snapshot_text(&self) -> String {
        let awareness = self.lock_awareness();
        let doc = awareness.doc();
        let text = doc.get_or_insert_text(CONTENT_FIELD);
        // Bind to a local so the read transaction temporary is dropped at the
        // `;` (before the mutex guard), not carried to the end of the block.
        let contents = text.get_string(&doc.transact());
        contents
    }
}

/// Registry of all live [`Room`]s, keyed by note id. Cheap to clone.
#[derive(Clone, Default)]
pub struct RoomRegistry {
    rooms: Arc<DashMap<String, Arc<Room>>>,
}

impl RoomRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Get the room for `note_id`, creating and seeding it from disk if it
    /// doesn't exist yet, and register a new connection against it.
    ///
    /// The connection counter is incremented **while holding the map's
    /// shard lock**, so it can never race the reaper's removal check (which
    /// also runs under that lock) — a room observed with `connections == 0`
    /// by the reaper genuinely has no attached (or attaching) sockets.
    ///
    /// The caller MUST balance this with exactly one [`RoomRegistry::release`]
    /// when the connection ends (the WS handler does so via a drop guard).
    pub fn get_or_create(&self, note_id: &str, user_id: &str, state: &AppState) -> Arc<Room> {
        match self.rooms.entry(note_id.to_string()) {
            Entry::Occupied(entry) => {
                let room = entry.get();
                room.connections.fetch_add(1, Ordering::AcqRel);
                room.clone()
            }
            Entry::Vacant(entry) => {
                let (room, dirty_rx) = build_room(note_id, user_id, state);
                room.connections.fetch_add(1, Ordering::AcqRel);
                super::persist::spawn_persistence(Arc::downgrade(&room), state.clone(), dirty_rx);
                entry.insert(room.clone());
                room
            }
        }
    }

    /// Detach one connection. If it was the last, schedule a grace-period
    /// reaper that performs a final flush and removes the room — but only if
    /// no new connection has arrived by then.
    pub fn release(&self, room: Arc<Room>, state: AppState) {
        let prev = room.connections.fetch_sub(1, Ordering::AcqRel);
        if prev != 1 {
            return; // still has other connections
        }

        let registry = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(REAP_GRACE).await;
            if room.connections.load(Ordering::Acquire) != 0 {
                return; // someone reconnected during the grace period
            }
            // Final flush before removal so no edits are lost.
            if let Err(err) = super::persist::flush_room(&room, &state).await {
                tracing::error!(
                    note_id = %room.note_id,
                    error = %err,
                    "final flush of idle collab room failed",
                );
            }
            // Remove only if still empty — the predicate runs under the
            // shard lock, mutually exclusive with `get_or_create`'s
            // increment, so this can't drop a room that just got a socket.
            registry.rooms.remove_if(&room.note_id, |_, r| {
                r.connections.load(Ordering::Acquire) == 0
            });
        });
    }

    /// Best-effort flush of every live room. Used by the graceful-shutdown
    /// path so in-flight collab edits reach git before the process exits.
    pub async fn flush_all(&self, state: &AppState) {
        let rooms: Vec<Arc<Room>> = self.rooms.iter().map(|r| r.value().clone()).collect();
        let count = rooms.len();
        for room in rooms {
            if let Err(err) = super::persist::flush_room(&room, state).await {
                tracing::error!(
                    note_id = %room.note_id,
                    error = %err,
                    "shutdown flush of collab room failed",
                );
            }
        }
        if count > 0 {
            tracing::info!(count, "flushed live collab rooms on shutdown");
        }
    }

    /// Number of live rooms (used in tests).
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.rooms.len()
    }

    /// Fetch an existing room without creating one. Used to read a note's
    /// live in-memory text (`Room::snapshot_text`) when one is joined, so
    /// readers see edits that haven't been flushed to disk yet.
    pub fn get(&self, note_id: &str) -> Option<Arc<Room>> {
        self.rooms.get(note_id).map(|r| r.value().clone())
    }
}

/// Construct (but do not register) a room for `note_id`, seeding its doc's
/// `"content"` text from the note's current on-disk markdown.
fn build_room(
    note_id: &str,
    user_id: &str,
    state: &AppState,
) -> (Arc<Room>, mpsc::UnboundedReceiver<()>) {
    let doc = Doc::new();
    {
        let text = doc.get_or_insert_text(CONTENT_FIELD);
        let seed = state
            .notes_repo
            .read_file(&note_id_to_path(note_id))
            .ok()
            .flatten()
            .unwrap_or_default();
        if !seed.is_empty() {
            let mut txn = doc.transact_mut();
            text.insert(&mut txn, 0, &seed);
        }
    }

    let (broadcast_tx, _rx) = broadcast::channel(BROADCAST_CAPACITY);
    let (dirty_tx, dirty_rx) = mpsc::unbounded_channel();

    let room = Arc::new(Room {
        note_id: note_id.to_string(),
        awareness: Mutex::new(Awareness::new(doc)),
        broadcast: broadcast_tx,
        connections: AtomicUsize::new(0),
        dirty: AtomicBool::new(false),
        dirty_tx,
        owner_hint: user_id.to_string(),
        next_conn_id: AtomicU64::new(1),
    });

    (room, dirty_rx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use yrs::updates::decoder::Decode;
    use yrs::updates::encoder::Encode;
    use yrs::{ReadTxn, Transact, Update};

    /// A doc-update applied to the room converges the room's doc, and a
    /// broadcast frame is fired to subscribers.
    #[test]
    fn applying_update_converges_and_broadcasts() {
        let doc = Doc::new();
        let _ = doc.get_or_insert_text(CONTENT_FIELD);
        let (broadcast_tx, mut rx) = broadcast::channel::<RelayMsg>(16);
        let (dirty_tx, _dirty_rx) = mpsc::unbounded_channel();
        let room = Arc::new(Room {
            note_id: "t".to_string(),
            awareness: Mutex::new(Awareness::new(doc)),
            broadcast: broadcast_tx,
            connections: AtomicUsize::new(1),
            dirty: AtomicBool::new(false),
            dirty_tx,
            owner_hint: "admin".to_string(),
            next_conn_id: AtomicU64::new(1),
        });

        // A peer edits its own doc and produces an update.
        let peer = Doc::new();
        let peer_text = peer.get_or_insert_text(CONTENT_FIELD);
        {
            let mut txn = peer.transact_mut();
            peer_text.insert(&mut txn, 0, "# hello collab\n");
        }
        let update = {
            let sv = {
                let awareness = room.awareness.lock().unwrap();
                let sv = awareness.doc().transact().state_vector();
                sv
            };
            peer.transact().encode_state_as_update_v1(&sv)
        };

        // Apply to the room (as the WS handler would) and broadcast.
        {
            let awareness = room.awareness.lock().unwrap();
            let mut txn = awareness.doc().transact_mut();
            txn.apply_update(Update::decode_v1(&update).unwrap())
                .unwrap();
        }
        room.mark_dirty();
        let frame = yrs::sync::Message::Sync(yrs::sync::SyncMessage::Update(update)).encode_v1();
        room.broadcast_frame(7, Bytes::from(frame));

        assert_eq!(room.snapshot_text(), "# hello collab\n");
        assert!(room.dirty.load(Ordering::Acquire));

        let relayed = rx.try_recv().expect("a frame should have been broadcast");
        assert_eq!(relayed.origin, 7);
        assert!(!relayed.data.is_empty());
    }

    #[tokio::test]
    async fn get_or_create_seeds_from_disk_and_reuses_room() {
        let (state, _notes_dir, _db_dir) = crate::collab::test_support::test_state().await;
        state
            .notes_repo
            .write_and_commit("seed-note.md", "# seeded\n", "t", "t@e", "seed")
            .await
            .unwrap();

        let registry = RoomRegistry::new();
        let room = registry.get_or_create("seed-note", "admin", &state);
        assert_eq!(room.snapshot_text(), "# seeded\n");
        assert_eq!(room.connections.load(Ordering::Acquire), 1);

        // Second opener reuses the same room and bumps the counter.
        let room2 = registry.get_or_create("seed-note", "admin", &state);
        assert!(Arc::ptr_eq(&room, &room2));
        assert_eq!(room.connections.load(Ordering::Acquire), 2);
        assert_eq!(registry.len(), 1);
    }
}
