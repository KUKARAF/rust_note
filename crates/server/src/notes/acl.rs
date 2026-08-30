//! Access control for notes (owner, shared, public, etc.).
//!
//! Permission model: a note has exactly one `owner_id` (recorded in the
//! `notes` table when the note is first created) plus zero or more rows in
//! `note_access` granting a specific `user_id` either `"read"` or `"write"`
//! access. The owner implicitly has both read and write; a `"write"` grant
//! implies read as well.
//!
//! These helpers deliberately collapse "note doesn't exist" and "note
//! exists but user has no permission" into `Ok(false)` — route handlers
//! that need to tell the two apart (to return 404 vs 403) should do their
//! own existence check first (e.g. via `NotesRepo::read_file` or a `notes`
//! table lookup) rather than relying on these functions to distinguish it.

use sqlx::SqlitePool;

/// Row shape shared by `can_read`/`can_write`.
struct NoteRow {
    owner_id: String,
}

async fn fetch_note(db: &SqlitePool, note_id: &str) -> anyhow::Result<Option<NoteRow>> {
    let row: Option<(String,)> = sqlx::query_as("SELECT owner_id FROM notes WHERE id = ?")
        .bind(note_id)
        .fetch_optional(db)
        .await?;
    Ok(row.map(|(owner_id,)| NoteRow { owner_id }))
}

/// True if `user_id` may read the note identified by `note_id`: owner, or
/// any `note_access` grant (`"read"` or `"write"`). `Ok(false)` if the note
/// has no `notes` row at all, or the user has no grant.
pub async fn can_read(db: &SqlitePool, note_id: &str, user_id: &str) -> anyhow::Result<bool> {
    let Some(note) = fetch_note(db, note_id).await? else {
        return Ok(false);
    };
    if note.owner_id == user_id {
        return Ok(true);
    }

    let row: Option<(String,)> =
        sqlx::query_as("SELECT permission FROM note_access WHERE note_id = ? AND user_id = ?")
            .bind(note_id)
            .bind(user_id)
            .fetch_optional(db)
            .await?;

    Ok(matches!(row, Some((perm,)) if perm == "read" || perm == "write"))
}

/// True if `user_id` may write the note identified by `note_id`: owner, or
/// a `note_access` grant with `permission = "write"`.
pub async fn can_write(db: &SqlitePool, note_id: &str, user_id: &str) -> anyhow::Result<bool> {
    let Some(note) = fetch_note(db, note_id).await? else {
        return Ok(false);
    };
    if note.owner_id == user_id {
        return Ok(true);
    }

    let row: Option<(String,)> =
        sqlx::query_as("SELECT permission FROM note_access WHERE note_id = ? AND user_id = ?")
            .bind(note_id)
            .bind(user_id)
            .fetch_optional(db)
            .await?;

    Ok(matches!(row, Some((perm,)) if perm == "write"))
}

/// True if `user_id` is the recorded owner of `note_id` (used for
/// owner-only actions like deletion, distinct from `can_write`).
pub async fn is_owner(db: &SqlitePool, note_id: &str, user_id: &str) -> anyhow::Result<bool> {
    let note = fetch_note(db, note_id).await?;
    Ok(note.is_some_and(|n| n.owner_id == user_id))
}

/// True if `note_id` has a `notes` row at all (i.e. is registered/exists).
pub async fn note_exists(db: &SqlitePool, note_id: &str) -> anyhow::Result<bool> {
    Ok(fetch_note(db, note_id).await?.is_some())
}

/// Adopt an orphaned note: register `note_id` to `user_id` ONLY when it has
/// no DB row yet.
///
/// Orphans exist because the notes directory is a real, externally-written
/// vault (Obsidian/Syncthing/vimwiki write straight into the mounted repo),
/// while the app's DB only learns about notes created through its own API.
/// Adoption happens lazily on read paths (list / single GET / reindex): the
/// first authenticated user to see an orphan becomes its owner. On the
/// intended single-user deployment that is always the vault's owner; in a
/// multi-user setup, externally-created files are claimed by whoever lists
/// first — acceptable because only the deployment operator can write into
/// the mounted vault from outside the app.
pub async fn adopt_if_orphaned(
    db: &SqlitePool,
    note_id: &str,
    user_id: &str,
) -> anyhow::Result<()> {
    if fetch_note(db, note_id).await?.is_none() {
        ensure_note_registered(db, note_id, user_id).await?;
    }
    Ok(())
}

/// Register a newly-created note's ownership. A no-op if the note is
/// already registered (re-saving an existing note must not change its
/// owner).
pub async fn ensure_note_registered(
    db: &SqlitePool,
    note_id: &str,
    owner_id: &str,
) -> anyhow::Result<()> {
    let now = now_rfc3339();
    sqlx::query(
        "INSERT OR IGNORE INTO notes (id, owner_id, created_at, updated_at) VALUES (?, ?, ?, ?)",
    )
    .bind(note_id)
    .bind(owner_id)
    .bind(&now)
    .bind(&now)
    .execute(db)
    .await?;
    Ok(())
}

/// Record that `note_id` was just modified, updating its `updated_at`
/// timestamp. Called after every successful write (REST PUT/POST, collab
/// flush) so the note-list endpoint can report last-modified cheaply without
/// walking git history (dos-01/dos-02). A no-op if the row doesn't exist.
pub async fn touch_updated_at(db: &SqlitePool, note_id: &str) -> anyhow::Result<()> {
    let now = now_rfc3339();
    sqlx::query("UPDATE notes SET updated_at = ? WHERE id = ?")
        .bind(now)
        .bind(note_id)
        .execute(db)
        .await?;
    Ok(())
}

/// Remove a note's `notes` row and any `note_access` grants for it (used on
/// deletion). Also drops any stored CRDT continuity blob so a re-created note
/// with the same id doesn't resurrect the old note's collab state.
pub async fn deregister_note(db: &SqlitePool, note_id: &str) -> anyhow::Result<()> {
    sqlx::query("DELETE FROM note_access WHERE note_id = ?")
        .bind(note_id)
        .execute(db)
        .await?;
    delete_note_crdt(db, note_id).await?;
    sqlx::query("DELETE FROM notes WHERE id = ?")
        .bind(note_id)
        .execute(db)
        .await?;
    Ok(())
}

/// Load the stored Yjs CRDT continuity blob for `note_id`, if any.
///
/// Used by `collab::room::build_room` to rebuild a reaped room's doc from its
/// prior CRDT state (preserving item/client ids) instead of re-seeding from
/// plain markdown, which would mint a fresh CRDT identity and duplicate text
/// against a client that kept its own doc. `None` when the note has never
/// been collaboratively edited, or its blob was invalidated by an out-of-band
/// write (see [`delete_note_crdt`]).
pub async fn load_note_crdt(db: &SqlitePool, note_id: &str) -> anyhow::Result<Option<Vec<u8>>> {
    let row: Option<(Vec<u8>,)> = sqlx::query_as("SELECT state FROM note_crdt WHERE note_id = ?")
        .bind(note_id)
        .fetch_optional(db)
        .await?;
    Ok(row.map(|(state,)| state))
}

/// Persist (upsert) the Yjs CRDT continuity blob for `note_id`. Called on
/// every collab flush so a room that is later reaped can be rebuilt with the
/// same CRDT identity.
pub async fn save_note_crdt(db: &SqlitePool, note_id: &str, state: &[u8]) -> anyhow::Result<()> {
    let now = now_rfc3339();
    sqlx::query(
        "INSERT INTO note_crdt (note_id, state, updated_at) VALUES (?, ?, ?) \
         ON CONFLICT(note_id) DO UPDATE SET state = excluded.state, updated_at = excluded.updated_at",
    )
    .bind(note_id)
    .bind(state)
    .bind(&now)
    .execute(db)
    .await?;
    Ok(())
}

/// Invalidate a note's stored CRDT blob. Called when the note file is written
/// outside collab (REST PUT/POST) so the disk markdown becomes the source of
/// truth again and the next room build re-seeds from it rather than from a now
/// -stale blob. A no-op when no blob exists.
pub async fn delete_note_crdt(db: &SqlitePool, note_id: &str) -> anyhow::Result<()> {
    sqlx::query("DELETE FROM note_crdt WHERE note_id = ?")
        .bind(note_id)
        .execute(db)
        .await?;
    Ok(())
}

/// Grant `permission` (`"read"` or `"write"`) on `note_id` to `user_id`.
///
/// Not yet wired into a route (a "share with user" endpoint is a
/// reasonable follow-up), but exercised directly by the ACL tests in
/// `notes::routes::tests`; `#[allow(dead_code)]` silences the non-test
/// build's dead-code warning in the meantime.
#[allow(dead_code)]
pub async fn grant_access(
    db: &SqlitePool,
    note_id: &str,
    user_id: &str,
    permission: &str,
) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO note_access (note_id, user_id, permission) VALUES (?, ?, ?) \
         ON CONFLICT(note_id, user_id) DO UPDATE SET permission = excluded.permission",
    )
    .bind(note_id)
    .bind(user_id)
    .bind(permission)
    .execute(db)
    .await?;
    Ok(())
}

/// Revoke any access grant `user_id` has on `note_id`.
#[allow(dead_code)]
pub async fn revoke_access(db: &SqlitePool, note_id: &str, user_id: &str) -> anyhow::Result<()> {
    sqlx::query("DELETE FROM note_access WHERE note_id = ? AND user_id = ?")
        .bind(note_id)
        .bind(user_id)
        .execute(db)
        .await?;
    Ok(())
}

fn now_rfc3339() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}
