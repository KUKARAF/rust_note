//! Persistence for share tokens/links, backed by the `share_links` sqlite
//! table (see `migrations/0001_init.sql`):
//! `token TEXT PRIMARY KEY, note_id TEXT, owner_id TEXT, permission TEXT,
//! created_at TEXT, expires_at TEXT`.
//!
//! "Never expires" is encoded as a far-future sentinel timestamp
//! ([`NEVER_EXPIRES_SENTINEL`]) rather than a nullable `expires_at`, since
//! the column is `NOT NULL` in the already-shipped migration and adding a
//! new migration just to make it nullable isn't worth it. [`ShareLink`]
//! always carries the sentinel internally; the REST layer (`share::mod`)
//! translates it back to `expires_at: null` / "never" at the JSON boundary
//! so the sentinel never leaks to a client.

use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use rust_note_core::share_token;

/// Sentinel `expires_at` value meaning "never expires" — see module docs.
pub const NEVER_EXPIRES_SENTINEL: &str = "9999-12-31T23:59:59Z";

/// What a share link grants a guest who holds the token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SharePermission {
    /// Read-only: live view of edits, no write access (collab doc mutations
    /// from this connection are ignored, same mechanism as a read-only
    /// authenticated collaborator — see `collab::ws`).
    View,
    /// Full collaborative read/write access.
    Edit,
}

impl SharePermission {
    pub fn as_str(self) -> &'static str {
        match self {
            SharePermission::View => "view",
            SharePermission::Edit => "edit",
        }
    }

    fn from_db(s: &str) -> Option<Self> {
        match s {
            "view" => Some(SharePermission::View),
            "edit" => Some(SharePermission::Edit),
            _ => None,
        }
    }
}

/// TTL choices accepted by the create-share API (`expires_in` field).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShareTtl {
    Hours24,
    Days14,
    Never,
}

impl ShareTtl {
    /// Parse the `expires_in` request field: `"24h"`, `"14d"`, or `"never"`.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "24h" => Some(ShareTtl::Hours24),
            "14d" => Some(ShareTtl::Days14),
            "never" => Some(ShareTtl::Never),
            _ => None,
        }
    }

    fn expires_at(self) -> String {
        match self {
            ShareTtl::Never => NEVER_EXPIRES_SENTINEL.to_string(),
            ShareTtl::Hours24 => rfc3339_from_now(time::Duration::hours(24)),
            ShareTtl::Days14 => rfc3339_from_now(time::Duration::days(14)),
        }
    }
}

fn rfc3339_from_now(d: time::Duration) -> String {
    (OffsetDateTime::now_utc() + d)
        .format(&Rfc3339)
        .unwrap_or_default()
}

fn now_rfc3339() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_default()
}

/// A share-link row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShareLink {
    pub token: String,
    pub note_id: String,
    pub owner_id: String,
    pub permission: SharePermission,
    pub created_at: String,
    /// RFC3339 UTC, or [`NEVER_EXPIRES_SENTINEL`] for a never-expiring link.
    pub expires_at: String,
}

impl ShareLink {
    /// Whether this link is no longer usable (expired, or malformed
    /// timestamp data — which fails closed as "expired").
    pub fn is_expired(&self) -> bool {
        match OffsetDateTime::parse(&self.expires_at, &Rfc3339) {
            Ok(expires) => OffsetDateTime::now_utc() >= expires,
            Err(_) => true,
        }
    }
}

type Row = (String, String, String, String, String, String);

fn row_to_link(row: Row) -> Option<ShareLink> {
    let (token, note_id, owner_id, permission, created_at, expires_at) = row;
    Some(ShareLink {
        token,
        note_id,
        owner_id,
        permission: SharePermission::from_db(&permission)?,
        created_at,
        expires_at,
    })
}

/// Create a new share link for `note_id`, owned by `owner_id`. The caller is
/// responsible for having already verified `owner_id` actually owns the note
/// (this function does no ACL check itself).
pub async fn create(
    db: &SqlitePool,
    note_id: &str,
    owner_id: &str,
    permission: SharePermission,
    ttl: ShareTtl,
) -> anyhow::Result<ShareLink> {
    let token = share_token::generate();
    let created_at = now_rfc3339();
    let expires_at = ttl.expires_at();

    sqlx::query(
        "INSERT INTO share_links (token, note_id, owner_id, permission, created_at, expires_at) \
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(&token)
    .bind(note_id)
    .bind(owner_id)
    .bind(permission.as_str())
    .bind(&created_at)
    .bind(&expires_at)
    .execute(db)
    .await?;

    Ok(ShareLink {
        token,
        note_id: note_id.to_string(),
        owner_id: owner_id.to_string(),
        permission,
        created_at,
        expires_at,
    })
}

/// Resolve `token` to its share link, but only if it exists AND is not
/// expired. Both "no such token" and "expired token" collapse to `None` —
/// callers (the guest resolve route, the guest WS handler) return the same
/// 404 for both, so a guest can't use response-code differences to confirm a
/// token *used to* be valid.
pub async fn resolve(db: &SqlitePool, token: &str) -> anyhow::Result<Option<ShareLink>> {
    let Some(link) = find_any(db, token).await? else {
        return Ok(None);
    };
    if link.is_expired() {
        return Ok(None);
    }
    Ok(Some(link))
}

/// Fetch a share link by token regardless of expiry. Used by [`resolve`] and
/// by the owner-facing revoke route, which needs to tell "no such token"
/// (404) apart from "not yours" (403) even for an already-expired link.
pub async fn find_any(db: &SqlitePool, token: &str) -> anyhow::Result<Option<ShareLink>> {
    let row: Option<Row> = sqlx::query_as(
        "SELECT token, note_id, owner_id, permission, created_at, expires_at \
         FROM share_links WHERE token = ?",
    )
    .bind(token)
    .fetch_optional(db)
    .await?;

    Ok(row.and_then(row_to_link))
}

/// All share links `owner_id` has created for `note_id` (active and
/// expired alike — the caller/UI decides what to show), newest first.
pub async fn list_for_note(
    db: &SqlitePool,
    note_id: &str,
    owner_id: &str,
) -> anyhow::Result<Vec<ShareLink>> {
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT token, note_id, owner_id, permission, created_at, expires_at \
         FROM share_links WHERE note_id = ? AND owner_id = ? ORDER BY created_at DESC",
    )
    .bind(note_id)
    .bind(owner_id)
    .fetch_all(db)
    .await?;

    Ok(rows.into_iter().filter_map(row_to_link).collect())
}

/// Revoke (delete) `token`, but only if `owner_id` actually owns it. Returns
/// `true` if a row was deleted.
pub async fn revoke(db: &SqlitePool, token: &str, owner_id: &str) -> anyhow::Result<bool> {
    let result = sqlx::query("DELETE FROM share_links WHERE token = ? AND owner_id = ?")
        .bind(token)
        .bind(owner_id)
        .execute(db)
        .await?;
    Ok(result.rows_affected() > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn test_db() -> (SqlitePool, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let pool = crate::db::init_pool(db_path.to_str().unwrap())
            .await
            .unwrap();
        (pool, dir)
    }

    #[tokio::test]
    async fn create_resolve_expiry_and_revoke_round_trip() {
        let (db, _dir) = test_db().await;

        let link = create(
            &db,
            "my-note",
            "alice",
            SharePermission::Edit,
            ShareTtl::Days14,
        )
        .await
        .unwrap();
        assert_eq!(link.note_id, "my-note");
        assert_eq!(link.owner_id, "alice");
        assert_eq!(link.permission, SharePermission::Edit);
        assert!(!link.is_expired());

        // Resolves back to the same link.
        let resolved = resolve(&db, &link.token).await.unwrap().unwrap();
        assert_eq!(resolved, link);

        // A garbage token resolves to nothing.
        assert!(resolve(&db, "not-a-real-token").await.unwrap().is_none());

        // Revoking as the wrong owner does nothing.
        assert!(!revoke(&db, &link.token, "bob").await.unwrap());
        assert!(resolve(&db, &link.token).await.unwrap().is_some());

        // Revoking as the real owner deletes it.
        assert!(revoke(&db, &link.token, "alice").await.unwrap());
        assert!(resolve(&db, &link.token).await.unwrap().is_none());
        // Idempotent: revoking again finds nothing left to delete.
        assert!(!revoke(&db, &link.token, "alice").await.unwrap());
    }

    #[tokio::test]
    async fn expired_link_resolves_to_none_but_find_any_still_sees_it() {
        let (db, _dir) = test_db().await;

        // Insert an already-expired link directly (ShareTtl can't produce
        // one, so bypass `create` and build the row by hand).
        let token = "expired-token-1234567890".to_string();
        sqlx::query(
            "INSERT INTO share_links (token, note_id, owner_id, permission, created_at, expires_at) \
             VALUES (?, 'note-a', 'alice', 'view', '2020-01-01T00:00:00Z', '2020-01-02T00:00:00Z')",
        )
        .bind(&token)
        .execute(&db)
        .await
        .unwrap();

        assert!(
            resolve(&db, &token).await.unwrap().is_none(),
            "an expired link must not resolve"
        );
        // But it's still visible to find_any / list, e.g. so the owner's
        // "manage shares" UI can show it as expired rather than it just
        // vanishing.
        let raw = find_any(&db, &token).await.unwrap().unwrap();
        assert!(raw.is_expired());

        let listed = list_for_note(&db, "note-a", "alice").await.unwrap();
        assert_eq!(listed.len(), 1);
        assert!(listed[0].is_expired());
    }

    #[tokio::test]
    async fn never_expires_sentinel_never_resolves_to_none() {
        let (db, _dir) = test_db().await;

        let link = create(
            &db,
            "forever-note",
            "alice",
            SharePermission::View,
            ShareTtl::Never,
        )
        .await
        .unwrap();
        assert_eq!(link.expires_at, NEVER_EXPIRES_SENTINEL);
        assert!(!link.is_expired());
        assert!(resolve(&db, &link.token).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn list_for_note_only_returns_the_given_owners_links() {
        let (db, _dir) = test_db().await;

        create(
            &db,
            "shared-note",
            "alice",
            SharePermission::View,
            ShareTtl::Hours24,
        )
        .await
        .unwrap();
        create(
            &db,
            "shared-note",
            "bob",
            SharePermission::Edit,
            ShareTtl::Hours24,
        )
        .await
        .unwrap();

        let alice_links = list_for_note(&db, "shared-note", "alice").await.unwrap();
        assert_eq!(alice_links.len(), 1);
        assert_eq!(alice_links[0].owner_id, "alice");

        let bob_links = list_for_note(&db, "shared-note", "bob").await.unwrap();
        assert_eq!(bob_links.len(), 1);
        assert_eq!(bob_links[0].owner_id, "bob");
    }
}
