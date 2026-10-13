//! MCP OAuth 2.1 Resource Server check: validates the bearer tokens issued
//! by [`super::oauth`] (rust_note's own, self-contained Authorization
//! Server for `/mcp` - see that module's doc comment for why it exists
//! instead of delegating to Authentik) and serves RFC 9728 Protected
//! Resource Metadata pointing clients at it.

use std::sync::Arc;

use axum::extract::Request;
use axum::http::{header, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

use super::oauth::store;
use crate::auth::device_token::bearer_from_headers;
use crate::config::Config;

/// The authenticated MCP user id, injected into request extensions by
/// [`McpAuth::require`] and read back by tool/resource handlers (through the
/// `http::request::Parts` the rmcp transport forwards into the request context).
#[derive(Clone, Debug)]
pub struct AuthUser(pub String);

/// MCP Resource-Server auth context: just enough to validate a bearer token
/// against the DB and to serve the PRM document.
pub struct McpAuth {
    dev_mode: bool,
    db: sqlx::SqlitePool,
    /// The RFC 9728 `resource` value (purely an identifier now - nothing
    /// validates a token `aud` against it, since there's no JWT).
    resource_uri: String,
    /// rust_note's own base URL - the RFC 9728 `authorization_servers`
    /// entry. rust_note is its own Authorization Server for `/mcp`, so this
    /// is just `config.base_url`, unconditionally.
    base_url: String,
    /// Absolute URL of the PRM document (for the `WWW-Authenticate` challenge).
    prm_url: String,
}

impl McpAuth {
    pub fn from_config(config: &Config, db: sqlx::SqlitePool) -> Arc<Self> {
        let prm_url = format!(
            "{}/.well-known/oauth-protected-resource",
            config.base_url.trim_end_matches('/')
        );
        Arc::new(Self {
            dev_mode: config.dev_mode,
            db,
            resource_uri: config.mcp_resource_uri.clone(),
            base_url: config.base_url.trim_end_matches('/').to_string(),
            prm_url,
        })
    }

    /// RFC 9728 Protected Resource Metadata (served unauthenticated).
    pub fn protected_resource_metadata(&self) -> Json<serde_json::Value> {
        Json(json!({
            "resource": self.resource_uri,
            "authorization_servers": [self.base_url],
            "bearer_methods_supported": ["header"],
        }))
    }

    /// A `401` challenge pointing the client at the PRM document.
    fn challenge(&self) -> Response {
        let mut resp = (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "unauthorized" })),
        )
            .into_response();
        let value = format!("Bearer resource_metadata=\"{}\"", self.prm_url);
        let header_value =
            HeaderValue::from_str(&value).unwrap_or_else(|_| HeaderValue::from_static("Bearer"));
        resp.headers_mut()
            .insert(header::WWW_AUTHENTICATE, header_value);
        resp
    }

    /// axum middleware body: authenticate the request and stash [`AuthUser`], or
    /// return a `401` challenge.
    ///
    /// In dev mode auth is bypassed with the fixed `admin` user — the same
    /// global bypass `RequireAuth` uses; dev mode is loopback-only and
    /// fail-validated at boot, so this never weakens a real deployment.
    pub async fn require(&self, mut req: Request, next: Next) -> Response {
        if self.dev_mode {
            req.extensions_mut()
                .insert(AuthUser(crate::config::DEV_MODE_USER_ID.to_string()));
            return next.run(req).await;
        }
        let Some(token) = bearer_from_headers(req.headers()) else {
            return self.challenge();
        };
        match store::resolve_access_token(&self.db, &token).await {
            Ok(Some(user_id)) => {
                req.extensions_mut().insert(AuthUser(user_id));
                next.run(req).await
            }
            Ok(None) => self.challenge(),
            Err(err) => {
                tracing::error!(error = %err, "mcp token validation: DB error");
                self.challenge()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    async fn test_pool() -> (tempfile::TempDir, sqlx::SqlitePool) {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let pool = crate::db::init_pool(db_path.to_str().unwrap())
            .await
            .unwrap();
        (dir, pool)
    }

    fn test_auth(db: sqlx::SqlitePool) -> Arc<McpAuth> {
        let mut cfg = Config::from_env();
        cfg.base_url = "https://notes.example.com".to_string();
        cfg.mcp_resource_uri = "https://notes.example.com/mcp".to_string();
        McpAuth::from_config(&cfg, db)
    }

    #[tokio::test]
    async fn prm_advertises_resource_and_self_as_authorization_server() {
        let (_dir, db) = test_pool().await;
        let auth = test_auth(db);
        let Json(doc) = auth.protected_resource_metadata();
        assert_eq!(doc["resource"], "https://notes.example.com/mcp");
        assert_eq!(doc["authorization_servers"][0], "https://notes.example.com");
        assert_eq!(doc["bearer_methods_supported"][0], "header");
    }

    #[tokio::test]
    async fn unknown_bearer_token_is_rejected() {
        let (_dir, db) = test_pool().await;
        let resolved = store::resolve_access_token(&db, "not-a-real-token")
            .await
            .unwrap();
        assert!(resolved.is_none());
        let _ = test_auth(db); // exercises construction with this pool too
    }
}
