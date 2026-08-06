//! Authentication and authorization.
//!
//! `oidc` mounts the `/auth/*` routes (login/callback/logout/me), `session`
//! provides the `tower-sessions`-backed session helpers and the
//! `RequireAuth` extractor future protected routes should use, and
//! `share_identity` is the (not-yet-wired-up) guest identity primitive for
//! the future share-link feature.

pub mod oidc;
pub mod session;
pub mod share_identity;
