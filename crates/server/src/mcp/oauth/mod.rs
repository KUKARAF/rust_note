//! rust_note's own, self-contained OAuth 2.1 Authorization Server for
//! `/mcp`. See [`crate::mcp`]'s module doc for why this exists: Authentik
//! (the app's OIDC provider for everything else) has no RFC 7591 Dynamic
//! Client Registration support, so connecting a new MCP client would
//! otherwise require hand-creating a client in Authentik's admin UI every
//! time. Authentik's configuration is never read, written, or depended on
//! by anything in this module - it is entirely independent, using the same
//! `/auth/login` session cookie (already backed by Authentik) only to
//! answer "who is this human" at the consent step.

pub mod routes;
pub mod store;
