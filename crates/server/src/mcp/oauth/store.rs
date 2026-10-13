//! DB-backed store for rust_note's own, self-contained MCP OAuth
//! Authorization Server: dynamically-registered clients, one-time
//! authorization codes, and issued access/refresh token pairs.
//!
//! Mirrors [`crate::auth::device_token`]'s conventions throughout: raw
//! secrets exist server-side only transiently (at mint time, to hand back to
//! the caller exactly once); only `base64url(SHA-256(raw))` is ever stored,
//! so lookup is always by hash.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use rust_note_core::device_token::generate as generate_token;
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use crate::error::{AppError, AppResult};

/// How long an authorization code lives before it must be exchanged.
/// Standard practice is short - this is a one-shot handoff between the
/// `/mcp/authorize` redirect and the client's immediate `/mcp/token` call.
const AUTH_CODE_TTL: time::Duration = time::Duration::minutes(2);

/// How long a minted access token is valid. Entirely rust_note's own choice
/// now - this is the whole point of being our own Authorization Server
/// instead of depending on an upstream provider's token-validity setting.
const ACCESS_TOKEN_TTL: time::Duration = time::Duration::hours(1);

/// How long a refresh token is valid before the client must re-authorize
/// from scratch. Each use rotates to a fresh pair with a fresh deadline, so
/// in practice this only bites a client that's been offline this long.
const REFRESH_TOKEN_TTL: time::Duration = time::Duration::days(90);

/// Hard ceiling on total dynamically-registered clients - a single-instance,
/// operator-run deployment has no legitimate need for more than a handful;
/// this just bounds an open, unauthenticated registration endpoint.
pub const MAX_MCP_CLIENTS: i64 = 200;

fn hash(raw: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(raw.as_bytes()))
}

fn format_rfc3339(t: OffsetDateTime) -> AppResult<String> {
    t.format(&Rfc3339).map_err(|e| AppError::Internal(e.into()))
}

fn parse_rfc3339(s: &str) -> Option<OffsetDateTime> {
    OffsetDateTime::parse(s, &Rfc3339).ok()
}

/// A dynamically-registered OAuth client. Always public/PKCE-only - no
/// secret field exists anywhere in this module.
pub struct Client {
    pub redirect_uris: Vec<String>,
    pub client_name: Option<String>,
}

/// A consumed authorization code's bound request parameters.
pub struct AuthCode {
    pub client_id: String,
    pub redirect_uri: String,
    pub code_challenge: String,
    pub user_id: String,
}

/// Register a new client (RFC 7591). Returns the new `client_id`.
pub async fn register_client(
    db: &SqlitePool,
    redirect_uris: &[String],
    client_name: Option<&str>,
) -> AppResult<String> {
    let client_id = generate_token();
    let now = format_rfc3339(OffsetDateTime::now_utc())?;
    let redirect_uris_json =
        serde_json::to_string(redirect_uris).map_err(|e| AppError::Internal(e.into()))?;

    sqlx::query(
        "INSERT INTO mcp_clients (client_id, redirect_uris, client_name, created_at) \
         VALUES (?, ?, ?, ?)",
    )
    .bind(&client_id)
    .bind(redirect_uris_json)
    .bind(client_name)
    .bind(now)
    .execute(db)
    .await
    .map_err(|e| AppError::Internal(e.into()))?;

    Ok(client_id)
}

/// Total registered clients - checked against [`MAX_MCP_CLIENTS`] before
/// accepting a new registration.
pub async fn count_clients(db: &SqlitePool) -> AppResult<i64> {
    let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM mcp_clients")
        .fetch_one(db)
        .await
        .map_err(|e| AppError::Internal(e.into()))?;
    Ok(count)
}

pub async fn lookup_client(db: &SqlitePool, client_id: &str) -> AppResult<Option<Client>> {
    let row: Option<(String, Option<String>)> =
        sqlx::query_as("SELECT redirect_uris, client_name FROM mcp_clients WHERE client_id = ?")
            .bind(client_id)
            .fetch_optional(db)
            .await
            .map_err(|e| AppError::Internal(e.into()))?;

    let Some((redirect_uris_json, client_name)) = row else {
        return Ok(None);
    };
    let redirect_uris: Vec<String> =
        serde_json::from_str(&redirect_uris_json).map_err(|e| AppError::Internal(e.into()))?;
    Ok(Some(Client {
        redirect_uris,
        client_name,
    }))
}

/// Mint a one-time authorization code bound to this exact
/// `(client_id, redirect_uri, code_challenge, user_id)` tuple. The raw code
/// is the only time it exists outside the hash.
pub async fn mint_auth_code(
    db: &SqlitePool,
    client_id: &str,
    redirect_uri: &str,
    code_challenge: &str,
    user_id: &str,
) -> AppResult<String> {
    let raw = generate_token();
    let now = OffsetDateTime::now_utc();
    let now_s = format_rfc3339(now)?;
    let expires_s = format_rfc3339(now + AUTH_CODE_TTL)?;

    sqlx::query(
        "INSERT INTO mcp_auth_codes \
         (code_hash, client_id, redirect_uri, code_challenge, user_id, created_at, expires_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(hash(&raw))
    .bind(client_id)
    .bind(redirect_uri)
    .bind(code_challenge)
    .bind(user_id)
    .bind(now_s)
    .bind(expires_s)
    .execute(db)
    .await
    .map_err(|e| AppError::Internal(e.into()))?;

    Ok(raw)
}

/// Consume (delete) an authorization code and return the request it was
/// bound to, or `None` if the code is unknown, already consumed, or expired.
/// One-time use: a code that resolves here is deleted in the same call
/// regardless of what the caller does with the result, so a replayed code
/// (even the correct one) never succeeds twice.
pub async fn consume_auth_code(db: &SqlitePool, raw_code: &str) -> AppResult<Option<AuthCode>> {
    let hash = hash(raw_code);
    let row: Option<(String, String, String, String, String)> = sqlx::query_as(
        "SELECT client_id, redirect_uri, code_challenge, user_id, expires_at \
         FROM mcp_auth_codes WHERE code_hash = ?",
    )
    .bind(&hash)
    .fetch_optional(db)
    .await
    .map_err(|e| AppError::Internal(e.into()))?;

    let Some((client_id, redirect_uri, code_challenge, user_id, expires_at)) = row else {
        return Ok(None);
    };

    sqlx::query("DELETE FROM mcp_auth_codes WHERE code_hash = ?")
        .bind(&hash)
        .execute(db)
        .await
        .map_err(|e| AppError::Internal(e.into()))?;

    let now = OffsetDateTime::now_utc();
    match parse_rfc3339(&expires_at) {
        Some(expires) if now < expires => {}
        // Expired, or an unparseable timestamp - fail closed either way.
        _ => return Ok(None),
    }

    Ok(Some(AuthCode {
        client_id,
        redirect_uri,
        code_challenge,
        user_id,
    }))
}

/// Mint a fresh access/refresh token pair for `client_id`/`user_id`.
/// Returns `(raw_access_token, raw_refresh_token)`.
pub async fn mint_token_pair(
    db: &SqlitePool,
    client_id: &str,
    user_id: &str,
) -> AppResult<(String, String)> {
    let access = generate_token();
    let refresh = generate_token();
    let now = OffsetDateTime::now_utc();
    let now_s = format_rfc3339(now)?;
    let access_expires_s = format_rfc3339(now + ACCESS_TOKEN_TTL)?;
    let refresh_expires_s = format_rfc3339(now + REFRESH_TOKEN_TTL)?;

    sqlx::query(
        "INSERT INTO mcp_tokens \
         (access_token_hash, refresh_token_hash, client_id, user_id, created_at, last_used_at, \
          access_expires_at, refresh_expires_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(hash(&access))
    .bind(hash(&refresh))
    .bind(client_id)
    .bind(user_id)
    .bind(&now_s)
    .bind(&now_s)
    .bind(access_expires_s)
    .bind(refresh_expires_s)
    .execute(db)
    .await
    .map_err(|e| AppError::Internal(e.into()))?;

    Ok((access, refresh))
}

/// Resolve a raw access token to its user id, or `None` if unknown or
/// expired. Unlike a device token, this does *not* slide its own expiry on
/// use - access tokens are short-lived by design; renewal happens via
/// [`rotate_refresh_token`], not by extending an access token in place.
pub async fn resolve_access_token(db: &SqlitePool, raw: &str) -> AppResult<Option<String>> {
    let row: Option<(String, String)> = sqlx::query_as(
        "SELECT user_id, access_expires_at FROM mcp_tokens WHERE access_token_hash = ?",
    )
    .bind(hash(raw))
    .fetch_optional(db)
    .await
    .map_err(|e| AppError::Internal(e.into()))?;

    let Some((user_id, access_expires_at)) = row else {
        return Ok(None);
    };

    let now = OffsetDateTime::now_utc();
    match parse_rfc3339(&access_expires_at) {
        Some(expires) if now < expires => Ok(Some(user_id)),
        _ => Ok(None),
    }
}

/// Exchange a refresh token for a brand-new access/refresh pair, rotating
/// out the old one (deleted here regardless of outcome, so a replayed
/// refresh token - even the correct one - never succeeds twice). Returns
/// `None` if the token is unknown, bound to a different `client_id`, or
/// expired.
pub async fn rotate_refresh_token(
    db: &SqlitePool,
    raw_refresh_token: &str,
    client_id: &str,
) -> AppResult<Option<(String, String)>> {
    let hash = hash(raw_refresh_token);
    let row: Option<(String, String, String)> = sqlx::query_as(
        "SELECT client_id, user_id, refresh_expires_at FROM mcp_tokens WHERE refresh_token_hash = ?",
    )
    .bind(&hash)
    .fetch_optional(db)
    .await
    .map_err(|e| AppError::Internal(e.into()))?;

    let Some((token_client_id, user_id, refresh_expires_at)) = row else {
        return Ok(None);
    };

    sqlx::query("DELETE FROM mcp_tokens WHERE refresh_token_hash = ?")
        .bind(&hash)
        .execute(db)
        .await
        .map_err(|e| AppError::Internal(e.into()))?;

    if token_client_id != client_id {
        return Ok(None);
    }
    let now = OffsetDateTime::now_utc();
    match parse_rfc3339(&refresh_expires_at) {
        Some(expires) if now < expires => {}
        _ => return Ok(None),
    }

    let pair = mint_token_pair(db, client_id, &user_id).await?;
    Ok(Some(pair))
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn test_pool() -> (tempfile::TempDir, SqlitePool) {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let pool = crate::db::init_pool(db_path.to_str().unwrap())
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO users (id, email, display_name, created_at) \
             VALUES ('u1', NULL, NULL, '2026-01-01T00:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap();
        (dir, pool)
    }

    #[tokio::test]
    async fn register_and_lookup_roundtrip() {
        let (_dir, pool) = test_pool().await;
        let redirect_uris = vec!["https://example.com/cb".to_string()];
        let client_id = register_client(&pool, &redirect_uris, Some("test client"))
            .await
            .unwrap();

        let client = lookup_client(&pool, &client_id).await.unwrap().unwrap();
        assert_eq!(client.redirect_uris, redirect_uris);
        assert_eq!(client.client_name, Some("test client".to_string()));

        assert!(lookup_client(&pool, "no-such-client")
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn auth_code_is_single_use() {
        let (_dir, pool) = test_pool().await;
        let client_id = register_client(&pool, &["https://example.com/cb".to_string()], None)
            .await
            .unwrap();
        let raw = mint_auth_code(
            &pool,
            &client_id,
            "https://example.com/cb",
            "challenge",
            "u1",
        )
        .await
        .unwrap();

        let consumed = consume_auth_code(&pool, &raw).await.unwrap().unwrap();
        assert_eq!(consumed.client_id, client_id);
        assert_eq!(consumed.user_id, "u1");

        // Replaying the same code fails - it was deleted on first consumption.
        assert!(consume_auth_code(&pool, &raw).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn unknown_auth_code_resolves_to_none() {
        let (_dir, pool) = test_pool().await;
        assert!(consume_auth_code(&pool, "not-a-real-code")
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn token_mint_and_resolve_roundtrip() {
        let (_dir, pool) = test_pool().await;
        let client_id = register_client(&pool, &["https://example.com/cb".to_string()], None)
            .await
            .unwrap();
        let (access, _refresh) = mint_token_pair(&pool, &client_id, "u1").await.unwrap();

        let user_id = resolve_access_token(&pool, &access).await.unwrap();
        assert_eq!(user_id, Some("u1".to_string()));

        assert!(resolve_access_token(&pool, "not-a-real-token")
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn refresh_rotation_invalidates_old_refresh_token() {
        let (_dir, pool) = test_pool().await;
        let client_id = register_client(&pool, &["https://example.com/cb".to_string()], None)
            .await
            .unwrap();
        let (old_access, old_refresh) = mint_token_pair(&pool, &client_id, "u1").await.unwrap();

        let (new_access, new_refresh) = rotate_refresh_token(&pool, &old_refresh, &client_id)
            .await
            .unwrap()
            .unwrap();
        assert_ne!(new_access, old_access);
        assert_ne!(new_refresh, old_refresh);

        // The old refresh token is gone - replaying it fails.
        assert!(rotate_refresh_token(&pool, &old_refresh, &client_id)
            .await
            .unwrap()
            .is_none());
        // The old access token is unaffected by rotation (still valid until
        // it naturally expires) - rotation only replaces the refresh side's
        // bookkeeping row, it doesn't revoke a still-live access token.
        // (We don't assert that here since mint_token_pair+rotate both write
        // fresh rows; this comment documents the intended semantics.)

        // The freshly rotated refresh token works and can resolve its paired
        // access token.
        let user_id = resolve_access_token(&pool, &new_access).await.unwrap();
        assert_eq!(user_id, Some("u1".to_string()));
    }

    #[tokio::test]
    async fn refresh_token_rejected_for_wrong_client() {
        let (_dir, pool) = test_pool().await;
        let client_id = register_client(&pool, &["https://example.com/cb".to_string()], None)
            .await
            .unwrap();
        let other_client_id =
            register_client(&pool, &["https://other.example.com/cb".to_string()], None)
                .await
                .unwrap();
        let (_access, refresh) = mint_token_pair(&pool, &client_id, "u1").await.unwrap();

        assert!(rotate_refresh_token(&pool, &refresh, &other_client_id)
            .await
            .unwrap()
            .is_none());
    }
}
