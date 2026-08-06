-- Initial schema for rust_note.
--
-- Note: `tower-sessions-sqlx-store` manages its own `tower_sessions` table
-- automatically against this same pool/file; it is intentionally not
-- created here.

CREATE TABLE users (
    id TEXT PRIMARY KEY,
    email TEXT,
    display_name TEXT,
    created_at TEXT NOT NULL
);

CREATE TABLE notes (
    id TEXT PRIMARY KEY,
    owner_id TEXT NOT NULL REFERENCES users(id),
    created_at TEXT NOT NULL
);

CREATE TABLE note_access (
    note_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    permission TEXT NOT NULL,
    PRIMARY KEY (note_id, user_id)
);

CREATE TABLE share_links (
    token TEXT PRIMARY KEY,
    note_id TEXT NOT NULL,
    owner_id TEXT NOT NULL,
    permission TEXT NOT NULL,
    created_at TEXT NOT NULL,
    expires_at TEXT NOT NULL
);
