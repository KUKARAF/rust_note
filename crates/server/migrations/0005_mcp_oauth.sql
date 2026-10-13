-- rust_note's own, self-contained OAuth 2.1 Authorization Server for `/mcp`.
--
-- rust_note is the AS here - not Authentik - precisely so that connecting a
-- new MCP client (Claude Code, LiteLLM, claude.ai, ...) never requires
-- hand-creating anything in Authentik. Only PKCE-only "public" clients are
-- supported (no client secret is ever issued or stored); see
-- crate::mcp::oauth for the full flow.
--
-- Timestamps are RFC3339 TEXT, matching the existing tables. Only hashes of
-- codes/tokens are stored, mirroring device_tokens: the raw value exists
-- server-side only transiently, at mint time.

CREATE TABLE mcp_clients (
    client_id TEXT PRIMARY KEY,
    redirect_uris TEXT NOT NULL,   -- JSON array, validated at registration time
    client_name TEXT,
    created_at TEXT NOT NULL
);

-- One-time authorization codes. Deleted on consumption (or once expired);
-- a row existing at all means "not yet consumed".
CREATE TABLE mcp_auth_codes (
    code_hash TEXT PRIMARY KEY,
    client_id TEXT NOT NULL REFERENCES mcp_clients(client_id),
    redirect_uri TEXT NOT NULL,
    code_challenge TEXT NOT NULL,  -- S256 only; no code_challenge_method column needed
    user_id TEXT NOT NULL REFERENCES users(id),
    created_at TEXT NOT NULL,
    expires_at TEXT NOT NULL
);

-- Issued access/refresh token pairs, rotated as a unit on every refresh.
CREATE TABLE mcp_tokens (
    access_token_hash TEXT PRIMARY KEY,
    refresh_token_hash TEXT NOT NULL UNIQUE,
    client_id TEXT NOT NULL REFERENCES mcp_clients(client_id),
    user_id TEXT NOT NULL REFERENCES users(id),
    created_at TEXT NOT NULL,
    last_used_at TEXT NOT NULL,
    access_expires_at TEXT NOT NULL,
    refresh_expires_at TEXT NOT NULL
);

CREATE INDEX idx_mcp_auth_codes_expires ON mcp_auth_codes(expires_at);
CREATE INDEX idx_mcp_tokens_user ON mcp_tokens(user_id);
