-- Add an `updated_at` column to `notes` so the note-list endpoint can report
-- a cheap last-modified timestamp WITHOUT walking each note's full git
-- history (dos-01 / dos-02). It is maintained on every write (REST PUT/POST
-- and collab flush). Existing rows are backfilled from `created_at`.
--
-- SQLite's ALTER TABLE ADD COLUMN cannot use a non-constant default, so the
-- column is added nullable and then backfilled; code treats a NULL
-- `updated_at` as "fall back to created_at".
ALTER TABLE notes ADD COLUMN updated_at TEXT;
UPDATE notes SET updated_at = created_at WHERE updated_at IS NULL;
