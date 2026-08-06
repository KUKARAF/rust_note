//! Debounced git persistence of collaborative edits back to the notes store.
//!
//! Each room gets one long-lived persistence task (spawned by
//! [`super::room::RoomRegistry::get_or_create`]). The task holds only a
//! [`Weak`] reference to its room, so it never keeps an otherwise-idle room
//! alive; when the room is finally dropped, the task's dirty channel closes
//! and the task exits.
//!
//! # Debounce
//!
//! On the first edit the task waits out a quiet period ([`DEBOUNCE`]) —
//! reset by every subsequent edit — before flushing, so a burst of
//! keystrokes collapses into a single commit. A hard cap ([`MAX_INTERVAL`])
//! guarantees that a *continuously* edited note still flushes periodically
//! rather than never.
//!
//! # Consistency with REST
//!
//! A flush reads the doc's current text and writes it via
//! [`NotesRepo::write_and_commit`] while holding the per-note lock
//! (`state.note_locks`), exactly as the REST `PUT` handler does, so a collab
//! flush and a REST write can't interleave their git/DB effects. Flushes are
//! skipped when the doc text already matches disk, so idle rooms and
//! redundant wake-ups don't produce empty commits.

use std::sync::atomic::Ordering;
use std::sync::{Arc, Weak};
use std::time::Duration;

use tokio::sync::mpsc;

use super::room::Room;
use crate::notes::{acl, fs_store::note_id_to_path};
use crate::state::AppState;

/// Quiet period after the last edit before flushing.
const DEBOUNCE: Duration = Duration::from_secs(5);

/// Hard cap between flushes for a continuously-edited room.
const MAX_INTERVAL: Duration = Duration::from_secs(30);

/// Git author for collaborative flushes (edits are a CRDT merge of possibly
/// many users, so attribution is generic rather than per-user).
const COLLAB_AUTHOR_NAME: &str = "rust_note collab";
const COLLAB_AUTHOR_EMAIL: &str = "collab@rustnote.local";

/// Spawn the per-room debounced persistence loop.
pub fn spawn_persistence(
    room: Weak<Room>,
    state: AppState,
    mut dirty_rx: mpsc::UnboundedReceiver<()>,
) {
    tokio::spawn(async move {
        loop {
            // Block until there is something to flush. `None` means every
            // `dirty_tx` (held by the room) has been dropped, i.e. the room
            // is gone — nothing left to persist here (the reaper / shutdown
            // path performs the final flush before dropping the room).
            if dirty_rx.recv().await.is_none() {
                break;
            }

            // Debounce: flush once the edits go quiet for `DEBOUNCE`, or once
            // `MAX_INTERVAL` has elapsed since this batch's first edit,
            // whichever comes first.
            let hard_deadline = tokio::time::Instant::now() + MAX_INTERVAL;
            loop {
                tokio::select! {
                    _ = tokio::time::sleep(DEBOUNCE) => break,
                    _ = tokio::time::sleep_until(hard_deadline) => break,
                    signal = dirty_rx.recv() => {
                        if signal.is_none() {
                            // Room dropped mid-debounce. Try one last flush if
                            // it's somehow still upgradeable, then stop.
                            if let Some(room) = room.upgrade() {
                                let _ = flush_room(&room, &state).await;
                            }
                            return;
                        }
                        // Another edit arrived: loop, resetting the debounce
                        // sleep (a fresh `sleep(DEBOUNCE)` next iteration).
                    }
                }
            }

            let Some(room) = room.upgrade() else {
                break; // room gone
            };
            if let Err(err) = flush_room(&room, &state).await {
                tracing::error!(
                    note_id = %room.note_id,
                    error = %err,
                    "collab flush failed; will retry",
                );
                // Re-arm so the failed edits get retried after another
                // debounce window rather than being stranded until the next
                // edit. `flush_room` already left `dirty` set on write error.
                let _ = room.dirty_tx.send(());
            }
        }
    });
}

/// Flush a room's current text to git if it differs from disk.
///
/// Clears the room's dirty flag on success (or when already in sync). On a
/// write failure the dirty flag is left set so the edits are retried and
/// never lost. Holds the per-note lock across the disk read + git commit so
/// it is consistent with REST writes.
pub async fn flush_room(room: &Arc<Room>, state: &AppState) -> anyhow::Result<()> {
    // Claim the dirty flag up front; if nothing is dirty there's nothing to
    // do. A concurrent edit that sets it again after this point simply
    // schedules another flush.
    if !room.dirty.swap(false, Ordering::AcqRel) {
        return Ok(());
    }

    let text = room.snapshot_text();
    let note_id = &room.note_id;
    let rel_path = note_id_to_path(note_id);

    // Serialize against REST writes/deletes for this note id.
    let _note_guard = state.note_locks.lock(note_id).await;

    let current = state.notes_repo.read_file(&rel_path)?.unwrap_or_default();
    if current == text {
        return Ok(()); // already in sync; avoid an empty commit
    }

    // Normally the note already exists (the WS handler rejects opening a
    // note the user can't read, which implies a `notes` row). This is a
    // safety net for the new-note case: ensure the file we're about to
    // write is owned and therefore visible to the ACL-checked REST routes.
    acl::ensure_note_registered(&state.db, note_id, &room.owner_hint).await?;

    let message = format!("collab edit {note_id}");
    if let Err(err) = state
        .notes_repo
        .write_and_commit(
            &rel_path,
            &text,
            COLLAB_AUTHOR_NAME,
            COLLAB_AUTHOR_EMAIL,
            &message,
        )
        .await
    {
        // Keep the edits marked dirty so they are retried, not lost.
        room.dirty.store(true, Ordering::Release);
        return Err(err);
    }

    // Keep the note's list timestamp fresh even when it is edited purely via
    // live collab (never through a REST PUT), so `GET /api/notes` — which now
    // sources `updated_at` from the DB rather than git history — doesn't show
    // a stale time for actively-collaborated notes.
    let _ = acl::touch_updated_at(&state.db, note_id).await;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collab::room::CONTENT_FIELD;
    use crate::collab::test_support::test_state;
    use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize};
    use std::sync::Mutex;
    use tokio::sync::broadcast;
    use yrs::sync::Awareness;
    use yrs::{Doc, Text, Transact};

    fn room_with_text(note_id: &str, owner: &str, text: &str) -> Arc<Room> {
        let doc = Doc::new();
        {
            let t = doc.get_or_insert_text(CONTENT_FIELD);
            let mut txn = doc.transact_mut();
            t.insert(&mut txn, 0, text);
        }
        let (broadcast_tx, _rx) = broadcast::channel(16);
        let (dirty_tx, _dirty_rx) = mpsc::unbounded_channel();
        Arc::new(Room {
            note_id: note_id.to_string(),
            awareness: Mutex::new(Awareness::new(doc)),
            broadcast: broadcast_tx,
            connections: AtomicUsize::new(0),
            dirty: AtomicBool::new(true),
            dirty_tx,
            owner_hint: owner.to_string(),
            next_conn_id: AtomicU64::new(1),
        })
    }

    #[tokio::test]
    async fn dirty_room_flushes_content_to_disk_and_git() {
        let (state, _notes_dir, _db_dir) = test_state().await;

        // Pre-existing note owned by admin (mirrors the normal collab flow:
        // you only open notes that already exist).
        state
            .notes_repo
            .write_and_commit("flush-me.md", "# original\n", "admin", "a@e", "create")
            .await
            .unwrap();
        acl::ensure_note_registered(&state.db, "flush-me", "admin")
            .await
            .unwrap();

        let room = room_with_text("flush-me", "admin", "# original\nlive collab edit\n");
        flush_room(&room, &state).await.unwrap();

        assert_eq!(
            state.notes_repo.read_file("flush-me.md").unwrap().unwrap(),
            "# original\nlive collab edit\n"
        );
        assert!(
            !room.dirty.load(Ordering::Acquire),
            "dirty cleared on flush"
        );

        // A commit was recorded by the collab author.
        let history = state.notes_repo.history("flush-me.md").await.unwrap();
        assert_eq!(history[0].message, "collab edit flush-me");
        assert!(history[0].author.contains("collab@rustnote.local"));
    }

    #[tokio::test]
    async fn flush_is_noop_when_content_matches_disk() {
        let (state, _notes_dir, _db_dir) = test_state().await;
        state
            .notes_repo
            .write_and_commit("same.md", "identical\n", "admin", "a@e", "create")
            .await
            .unwrap();
        acl::ensure_note_registered(&state.db, "same", "admin")
            .await
            .unwrap();

        let before = state.notes_repo.history("same.md").await.unwrap().len();
        let room = room_with_text("same", "admin", "identical\n");
        flush_room(&room, &state).await.unwrap();

        // No new commit for identical content.
        let after = state.notes_repo.history("same.md").await.unwrap().len();
        assert_eq!(before, after, "identical content must not create a commit");
    }
}
