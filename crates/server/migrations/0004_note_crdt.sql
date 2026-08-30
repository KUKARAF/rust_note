-- Per-note Yjs CRDT state, so a collaborative-editing room can be torn down
-- and rebuilt (e.g. after the mobile WebView is frozen on screen-lock long
-- enough for the room to be reaped) without losing CRDT identity.
--
-- Before this table the server re-seeded a reaped room from the note's plain
-- markdown, minting brand-new CRDT ops with a fresh clientID. A client that
-- had kept its own doc (y-indexeddb, retained across sessions) would then
-- merge two independent insertions of the same text -> the CRDT keeps both ->
-- the note's text duplicated on every lock/unlock cycle.
--
-- The blob is `yrs` `encode_state_as_update_v1` over the full document state.
-- Plain markdown in git stays the human-readable source of truth (Obsidian /
-- Syncthing); this blob is an internal continuity cache and is invalidated
-- when the note file is written outside collab (REST PUT/POST), so the next
-- room build re-seeds from the newer disk text.
--
-- Timestamps are RFC3339 TEXT, matching the existing tables.

CREATE TABLE note_crdt (
    note_id    TEXT PRIMARY KEY REFERENCES notes(id),
    state      BLOB NOT NULL,   -- yrs encode_state_as_update_v1 (full state)
    updated_at TEXT NOT NULL
);
