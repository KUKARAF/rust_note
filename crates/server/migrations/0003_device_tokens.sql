-- Device tokens: long-lived bearer credentials for the mobile app.
--
-- The app's webview origin (http://tauri.localhost) is cross-site to the
-- server, so the SameSite=Lax session cookie is never sent on its fetches;
-- instead the app authenticates every request with `Authorization: Bearer`.
-- Only a SHA-256 hash of the token is stored - the raw token is delivered to
-- the client exactly once (in the /auth/callback redirect fragment) and never
-- persisted server-side.
--
-- Timestamps are RFC3339 TEXT, matching the existing tables.

CREATE TABLE device_tokens (
    token_hash TEXT PRIMARY KEY,           -- base64url(SHA-256(raw token))
    user_id TEXT NOT NULL REFERENCES users(id),
    label TEXT,                            -- e.g. 'android-app'
    created_at TEXT NOT NULL,
    last_used_at TEXT NOT NULL,
    expires_at TEXT NOT NULL
);

CREATE INDEX idx_device_tokens_user ON device_tokens(user_id);
