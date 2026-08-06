//! Device-token store: long-lived bearer credentials for the mobile app.
//!
//! The Tauri Android app's webview origin (`http://tauri.localhost`) is
//! cross-site to this server, so the `SameSite=Lax` session cookie is never
//! sent on its `fetch`es or WebSocket upgrades. Instead, the app completes
//! the normal OIDC login as top-level navigations (see
//! [`super::oidc`]'s `?client=app` flow), receives a device token in the
//! final redirect's URL *fragment*, and authenticates every subsequent
//! request with `Authorization: Bearer <token>` (REST) or `?token=` (collab
//! WebSocket — see `collab::ws`).
//!
//! # Storage & timing safety
//!
//! Only `base64url(SHA-256(raw))` is stored; the raw token exists
//! server-side only transiently in the login callback. Lookup is *by hash*:
//! the attacker-controlled value is hashed before it ever reaches the
//! database, so byte-wise B-tree comparison timing reveals nothing usable
//! about any stored token (exploiting it would require a SHA-256 preimage).
//! No constant-time comparison crate is needed.
//!
//! # Lifetime
//!
//! Tokens expire 90 days after last use (sliding, refreshed at most once an
//! hour to avoid a write per request). Logout with a bearer revokes the
//! token immediately; each user keeps at most [`MAX_TOKENS_PER_USER`] live
//! tokens (oldest evicted first).

use axum::http::HeaderMap;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use crate::error::{AppError, AppResult};

/// Sliding expiry: a token dies 90 days after its last (throttle-visible)
/// use. Long enough that a phone used even occasionally never re-runs the
/// OIDC dance; short enough that a lost device's token dies on its own.
const TOKEN_TTL: time::Duration = time::Duration::days(90);

/// How stale `last_used_at` must be before a successful resolve rewrites
/// it (and slides `expires_at`). Keeps the common case read-only.
const TOUCH_THROTTLE: time::Duration = time::Duration::hours(1);

/// Cap on live tokens per user; creating beyond it evicts the oldest.
const MAX_TOKENS_PER_USER: u32 = 10;

fn hash_token(raw: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(raw.as_bytes()))
}

fn format_rfc3339(t: OffsetDateTime) -> AppResult<String> {
    t.format(&Rfc3339).map_err(|e| AppError::Internal(e.into()))
}

/// Mint a new device token for `user_id`, returning the raw token (the only
/// time it ever exists server-side). Evicts the user's oldest tokens beyond
/// [`MAX_TOKENS_PER_USER`].
pub async fn create(db: &SqlitePool, user_id: &str, label: &str) -> AppResult<String> {
    let raw = rust_note_core::device_token::generate();
    let now = OffsetDateTime::now_utc();
    let now_s = format_rfc3339(now)?;
    let expires_s = format_rfc3339(now + TOKEN_TTL)?;

    sqlx::query(
        "INSERT INTO device_tokens (token_hash, user_id, label, created_at, last_used_at, expires_at) \
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(hash_token(&raw))
    .bind(user_id)
    .bind(label)
    .bind(&now_s)
    .bind(&now_s)
    .bind(&expires_s)
    .execute(db)
    .await
    .map_err(|e| AppError::Internal(e.into()))?;

    // Evict beyond the cap. `token_hash` as the ORDER BY tiebreaker keeps
    // eviction deterministic when several tokens share a created_at second.
    sqlx::query(
        "DELETE FROM device_tokens WHERE user_id = ? AND token_hash NOT IN ( \
             SELECT token_hash FROM device_tokens WHERE user_id = ? \
             ORDER BY created_at DESC, token_hash DESC LIMIT ? \
         )",
    )
    .bind(user_id)
    .bind(user_id)
    .bind(MAX_TOKENS_PER_USER)
    .execute(db)
    .await
    .map_err(|e| AppError::Internal(e.into()))?;

    Ok(raw)
}

/// Resolve a raw bearer token to its user id, or `None` if unknown or
/// expired (malformed stored timestamps fail closed as expired). On
/// success, slides the expiry forward — but only when `last_used_at` is
/// more than [`TOUCH_THROTTLE`] old, so steady traffic doesn't write on
/// every request.
pub async fn resolve(db: &SqlitePool, raw: &str) -> AppResult<Option<String>> {
    let hash = hash_token(raw);
    let row: Option<(String, String, String)> = sqlx::query_as(
        "SELECT user_id, last_used_at, expires_at FROM device_tokens WHERE token_hash = ?",
    )
    .bind(&hash)
    .fetch_optional(db)
    .await
    .map_err(|e| AppError::Internal(e.into()))?;

    let Some((user_id, last_used_at, expires_at)) = row else {
        return Ok(None);
    };

    let now = OffsetDateTime::now_utc();
    match OffsetDateTime::parse(&expires_at, &Rfc3339) {
        Ok(expires) if now < expires => {}
        // Expired — or unparseable, which fails closed as expired.
        _ => return Ok(None),
    }

    let needs_touch = match OffsetDateTime::parse(&last_used_at, &Rfc3339) {
        Ok(last_used) => now - last_used > TOUCH_THROTTLE,
        // Unparseable last_used_at: repair it by touching.
        Err(_) => true,
    };
    if needs_touch {
        let now_s = format_rfc3339(now)?;
        let expires_s = format_rfc3339(now + TOKEN_TTL)?;
        sqlx::query(
            "UPDATE device_tokens SET last_used_at = ?, expires_at = ? WHERE token_hash = ?",
        )
        .bind(&now_s)
        .bind(&expires_s)
        .bind(&hash)
        .execute(db)
        .await
        .map_err(|e| AppError::Internal(e.into()))?;
    }

    Ok(Some(user_id))
}

/// Delete the token (by raw value). A no-op if it doesn't exist — revoking
/// an already-revoked token is not an error.
pub async fn revoke(db: &SqlitePool, raw: &str) -> AppResult<()> {
    sqlx::query("DELETE FROM device_tokens WHERE token_hash = ?")
        .bind(hash_token(raw))
        .execute(db)
        .await
        .map_err(|e| AppError::Internal(e.into()))?;
    Ok(())
}

/// Extract the raw token from an `Authorization: Bearer <token>` header, if
/// present and well-formed (scheme matched case-insensitively per RFC 9110).
pub fn bearer_from_headers(headers: &HeaderMap) -> Option<String> {
    let value = headers
        .get(axum::http::header::AUTHORIZATION)?
        .to_str()
        .ok()?;
    let (scheme, token) = value.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return None;
    }
    let token = token.trim();
    if token.is_empty() {
        return None;
    }
    Some(token.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    async fn test_pool() -> (tempfile::TempDir, SqlitePool) {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let pool = crate::db::init_pool(db_path.to_str().unwrap())
            .await
            .unwrap();
        sqlx::query("INSERT INTO users (id, email, display_name, created_at) VALUES ('u1', NULL, NULL, '2026-01-01T00:00:00Z')")
            .execute(&pool)
            .await
            .unwrap();
        (dir, pool)
    }

    #[tokio::test]
    async fn create_resolve_roundtrip() {
        let (_dir, pool) = test_pool().await;
        let raw = create(&pool, "u1", "android-app").await.unwrap();
        assert_eq!(resolve(&pool, &raw).await.unwrap(), Some("u1".to_string()));
        assert_eq!(resolve(&pool, "not-a-real-token").await.unwrap(), None);
    }

    #[tokio::test]
    async fn expired_token_is_rejected() {
        let (_dir, pool) = test_pool().await;
        let raw = create(&pool, "u1", "android-app").await.unwrap();
        sqlx::query("UPDATE device_tokens SET expires_at = '2000-01-01T00:00:00Z'")
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(resolve(&pool, &raw).await.unwrap(), None);
    }

    #[tokio::test]
    async fn malformed_expiry_fails_closed() {
        let (_dir, pool) = test_pool().await;
        let raw = create(&pool, "u1", "android-app").await.unwrap();
        sqlx::query("UPDATE device_tokens SET expires_at = 'garbage'")
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(resolve(&pool, &raw).await.unwrap(), None);
    }

    #[tokio::test]
    async fn revoke_deletes_token() {
        let (_dir, pool) = test_pool().await;
        let raw = create(&pool, "u1", "android-app").await.unwrap();
        revoke(&pool, &raw).await.unwrap();
        assert_eq!(resolve(&pool, &raw).await.unwrap(), None);
        // Revoking again is a harmless no-op.
        revoke(&pool, &raw).await.unwrap();
    }

    #[tokio::test]
    async fn per_user_cap_evicts_oldest() {
        let (_dir, pool) = test_pool().await;
        let first = create(&pool, "u1", "android-app").await.unwrap();
        // Age the first token so eviction ordering is unambiguous even
        // within one created_at second.
        sqlx::query("UPDATE device_tokens SET created_at = '2020-01-01T00:00:00Z'")
            .execute(&pool)
            .await
            .unwrap();
        for _ in 0..MAX_TOKENS_PER_USER {
            create(&pool, "u1", "android-app").await.unwrap();
        }
        let (count,): (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM device_tokens WHERE user_id = 'u1'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(count, i64::from(MAX_TOKENS_PER_USER));
        assert_eq!(
            resolve(&pool, &first).await.unwrap(),
            None,
            "oldest token should have been evicted"
        );
    }

    #[tokio::test]
    async fn resolve_touch_is_throttled_and_slides_expiry() {
        let (_dir, pool) = test_pool().await;
        let raw = create(&pool, "u1", "android-app").await.unwrap();

        // Fresh token: a resolve within the throttle window must not write.
        let (before_used, before_expires): (String, String) =
            sqlx::query_as("SELECT last_used_at, expires_at FROM device_tokens")
                .fetch_one(&pool)
                .await
                .unwrap();
        resolve(&pool, &raw).await.unwrap();
        let (after_used, after_expires): (String, String) =
            sqlx::query_as("SELECT last_used_at, expires_at FROM device_tokens")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(
            (&before_used, &before_expires),
            (&after_used, &after_expires)
        );

        // Stale last_used_at: resolve must touch it and slide expires_at.
        sqlx::query("UPDATE device_tokens SET last_used_at = '2026-01-01T00:00:00Z'")
            .execute(&pool)
            .await
            .unwrap();
        resolve(&pool, &raw).await.unwrap();
        let (touched_used, touched_expires): (String, String) =
            sqlx::query_as("SELECT last_used_at, expires_at FROM device_tokens")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_ne!(touched_used, "2026-01-01T00:00:00Z");
        assert!(
            touched_expires > after_expires,
            "expiry should slide forward"
        );
    }

    #[test]
    fn bearer_header_parsing() {
        let mut headers = HeaderMap::new();
        assert_eq!(bearer_from_headers(&headers), None);

        headers.insert(
            axum::http::header::AUTHORIZATION,
            HeaderValue::from_static("Bearer abc123"),
        );
        assert_eq!(bearer_from_headers(&headers), Some("abc123".to_string()));

        headers.insert(
            axum::http::header::AUTHORIZATION,
            HeaderValue::from_static("bearer abc123"),
        );
        assert_eq!(
            bearer_from_headers(&headers),
            Some("abc123".to_string()),
            "scheme is case-insensitive"
        );

        headers.insert(
            axum::http::header::AUTHORIZATION,
            HeaderValue::from_static("Basic abc123"),
        );
        assert_eq!(bearer_from_headers(&headers), None);

        headers.insert(
            axum::http::header::AUTHORIZATION,
            HeaderValue::from_static("Bearer   "),
        );
        assert_eq!(bearer_from_headers(&headers), None, "empty token rejected");
    }
}
