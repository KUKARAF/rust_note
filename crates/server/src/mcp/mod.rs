//! Remote MCP (Model Context Protocol) server.
//!
//! Exposes a subset of the notes / todos / stats surface to LLM clients (e.g.
//! litellm) over the MCP **Streamable HTTP** transport, mounted at `/mcp` as a
//! Tower service on the main axum router (see [`crate::routes::build`]).
//!
//! Auth: rust_note is its own, self-contained OAuth 2.1 Authorization Server
//! for `/mcp` (see [`oauth`]) - it does **not** delegate to Authentik here.
//! Authentik has no support for OAuth Dynamic Client Registration (RFC
//! 7591), so every new MCP client (Claude Code, LiteLLM, claude.ai, ...)
//! would otherwise need a human to hand-create an OAuth client in
//! Authentik's admin UI before it could log in. Instead, [`oauth`] lets any
//! client register itself on the fly, entirely within rust_note's own
//! database - Authentik's configuration is never read, written, or
//! depended on by any of this. [`auth`] is the Resource-Server side that
//! validates the tokens [`oauth`] issues and maps them to the same
//! `user_id` the REST `RequireAuth` extractor yields, so every tool
//! enforces identical per-user ACLs.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::DefaultBodyLimit;
use axum::http::StatusCode;
use axum::routing::get;
use axum::Router;
use rmcp::transport::streamable_http_server::{
    session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
};
use tower_http::timeout::TimeoutLayer;

use crate::state::AppState;

mod auth;
mod oauth;
mod server;

use server::McpServer;

/// Body-size/timeout bounds for the DCR surface (`/mcp/register`,
/// `/mcp/authorize`, `/mcp/authorize/decision`, `/mcp/token`): one-shot
/// JSON/form requests that make at most one bounded DB round trip, not the
/// long-lived stream `/mcp` itself is deliberately mounted outside the REST
/// middleware stack for. Small and short rather than either the REST
/// stack's 4 MiB/30s (too generous for this payload shape) or no bound at
/// all (today's gap before this module existed).
const DCR_MAX_BODY_BYTES: usize = 16 * 1024;
const DCR_TIMEOUT: Duration = Duration::from_secs(10);

/// Build the MCP Streamable-HTTP Tower service.
///
/// A fresh [`McpServer`] is produced per session by the factory closure, each
/// capturing process-wide [`AppState`]. Request-scoped identity is injected
/// separately by the auth middleware (see [`auth`]).
fn service(state: AppState) -> StreamableHttpService<McpServer, LocalSessionManager> {
    // rmcp's DNS-rebinding guard defaults to loopback-only; behind a reverse
    // proxy the Host header is the deployment's public authority, so allow that
    // (plus loopback for local/dev use) or every request 403s.
    let config = StreamableHttpServerConfig::default()
        .with_allowed_hosts(allowed_hosts(&state.config.base_url));
    StreamableHttpService::new(
        move || Ok(McpServer::new(state.clone())),
        Arc::new(LocalSessionManager::default()),
        config,
    )
}

/// The Host-header allowlist for the MCP endpoint: the public authority derived
/// from `base_url` (with and without an explicit port) plus loopback forms.
fn allowed_hosts(base_url: &str) -> Vec<String> {
    let authority = base_url
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(base_url)
        .split('/')
        .next()
        .unwrap_or(base_url)
        .to_string();
    let host_only = authority
        .rsplit_once(':')
        .map(|(h, _)| h.to_string())
        .unwrap_or_else(|| authority.clone());
    let mut hosts = vec![
        authority,
        host_only,
        "localhost".to_string(),
        "127.0.0.1".to_string(),
        "[::1]".to_string(),
    ];
    hosts.dedup();
    hosts
}

/// Mount the MCP endpoint (`/mcp`, auth-gated), its unauthenticated OAuth
/// Protected Resource Metadata (`/.well-known/oauth-protected-resource`),
/// and the self-contained OAuth Authorization Server surface (RFC 7591
/// registration, the authorize/consent flow, the token endpoint, and RFC
/// 8414 AS metadata - see [`oauth`]) onto the application router.
pub fn mount(app: Router, state: AppState) -> Router {
    let mcp_service = service(state.clone());
    let mcp_auth = auth::McpAuth::from_config(&state.config, state.db.clone());

    // The MCP endpoint, wrapped in the Resource-Server auth middleware.
    let auth_for_mw = mcp_auth.clone();
    let mcp = Router::new()
        .nest_service("/mcp", mcp_service)
        .layer(axum::middleware::from_fn(move |req, next| {
            let auth = auth_for_mw.clone();
            async move { auth.require(req, next).await }
        }));

    // RFC 9728 Protected Resource Metadata — unauthenticated by design.
    let auth_for_prm = mcp_auth.clone();
    let well_known = Router::new().route(
        "/.well-known/oauth-protected-resource",
        get(move || {
            let auth = auth_for_prm.clone();
            async move { auth.protected_resource_metadata() }
        }),
    );

    // The DCR/authorize/token surface is unauthenticated at the HTTP layer
    // by design (a client has no token yet when registering/authorizing/
    // exchanging a code) - see `oauth::routes` module doc. Never nest this
    // inside `mcp`'s auth-wrapped router above.
    let dcr = oauth::routes::router()
        .layer(DefaultBodyLimit::max(DCR_MAX_BODY_BYTES))
        .layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            DCR_TIMEOUT,
        ))
        .with_state(state);

    app.merge(mcp).merge(well_known).merge(dcr)
}

#[cfg(test)]
mod tests {
    use super::allowed_hosts;

    #[test]
    fn allowed_hosts_from_https_base_url() {
        let hosts = allowed_hosts("https://notes.example.com");
        assert!(hosts.contains(&"notes.example.com".to_string()));
        // Loopback is always allowed too (local/dev use).
        assert!(hosts.contains(&"localhost".to_string()));
        assert!(hosts.contains(&"127.0.0.1".to_string()));
    }

    #[test]
    fn allowed_hosts_keeps_explicit_port() {
        let hosts = allowed_hosts("http://localhost:8080");
        // Both the authority-with-port and the bare host are allowed, since the
        // Host header may or may not carry the port.
        assert!(hosts.contains(&"localhost:8080".to_string()));
        assert!(hosts.contains(&"localhost".to_string()));
    }
}
