//! "Share identity": lightweight, non-OIDC identity for users accessing a
//! note via a share link rather than logging in through Authentik.
//!
//! [`GuestIdentity::new_random`] is used by `collab::ws`'s guest WebSocket
//! handler (`GET /ws/shared/{token}`) to mint each guest connection's cursor
//! color + display name. The cookie (de)serialization half
//! ([`GuestIdentity::to_cookie`] / [`GuestIdentity::from_cookie_value`]) is
//! not wired up yet — v1 mints a fresh identity per connection rather than a
//! reconnect-stable one; a future pass could persist it in a signed cookie
//! using the same signing mechanism as the OIDC login-flow cookie (see
//! `auth::oidc::flow_cookie_key`).

#![allow(dead_code)] // primitive only; not wired into routes yet (see module docs)

use axum_extra::extract::cookie::Cookie;
use serde::{Deserialize, Serialize};

/// Name of the cookie a guest identity is stored under.
pub const SHARE_IDENTITY_COOKIE: &str = "rustnote_guest";

/// A lightweight, non-authenticated identity used by share-link guests
/// (e.g. for presence/cursor colors in collaborative editing), distinct
/// from a real `users` row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuestIdentity {
    pub id: String,
    pub display_name: String,
    /// CSS `hsl(...)` color string, deterministic per `id`.
    pub color: String,
}

impl GuestIdentity {
    /// Mint a brand new guest identity with a random id.
    pub fn new_random() -> Self {
        let id = uuid::Uuid::new_v4().to_string();
        Self::from_id(id)
    }

    /// Build a guest identity from a specific id (used both by
    /// [`Self::new_random`] and when re-deriving display name/color for an
    /// id already read back from a cookie).
    pub fn from_id(id: String) -> Self {
        let display_name = format!("Guest-{}", last_hex_chars(&id, 4));
        let color = color_for_id(&id);
        Self {
            id,
            display_name,
            color,
        }
    }

    /// Build the (unsigned) `Cookie` for this identity. Actual signing
    /// happens when this is added to a `SignedCookieJar` (see
    /// `auth::oidc::flow_cookie_key` for the shared signing key), which is
    /// why this doesn't take a `Key` itself.
    pub fn to_cookie(&self) -> anyhow::Result<Cookie<'static>> {
        let value = serde_json::to_string(self)?;
        let mut cookie = Cookie::new(SHARE_IDENTITY_COOKIE, value);
        cookie.set_path("/");
        cookie.set_http_only(true);
        cookie.set_same_site(axum_extra::extract::cookie::SameSite::Lax);
        Ok(cookie)
    }

    /// Parse a guest identity back out of a cookie's value (already
    /// verified/decrypted by a `SignedCookieJar`).
    pub fn from_cookie_value(value: &str) -> Option<Self> {
        serde_json::from_str(value).ok()
    }
}

/// Last `n` hex-ish characters of a UUID string (strips hyphens first).
fn last_hex_chars(id: &str, n: usize) -> String {
    let stripped: String = id.chars().filter(|c| *c != '-').collect();
    let len = stripped.len();
    if len <= n {
        stripped
    } else {
        stripped[len - n..].to_string()
    }
}

/// FNV-1a hash, good enough (fast, well-distributed, no crypto needed) for
/// deriving a deterministic-but-unpredictable-looking color from an id.
fn fnv1a(bytes: &[u8]) -> u64 {
    const OFFSET_BASIS: u64 = 0xcbf29ce484222325;
    const PRIME: u64 = 0x100000001b3;
    let mut hash = OFFSET_BASIS;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

/// Deterministic `hsl(...)` CSS color string for a given id.
pub fn color_for_id(id: &str) -> String {
    let hash = fnv1a(id.as_bytes());
    let hue = hash % 360;
    format!("hsl({hue}, 65%, 55%)")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_is_deterministic_for_same_id() {
        let id = "11111111-2222-3333-4444-555555555555";
        assert_eq!(color_for_id(id), color_for_id(id));
    }

    #[test]
    fn different_ids_likely_produce_different_colors() {
        let colors: std::collections::HashSet<String> = (0..20)
            .map(|_| color_for_id(&uuid::Uuid::new_v4().to_string()))
            .collect();
        // Not a hard guarantee (hue space is only 360 wide), but with 20
        // random UUIDs collisions across the board would be exceedingly
        // unlikely if the hash is doing its job.
        assert!(colors.len() > 1);
    }

    #[test]
    fn guest_display_name_uses_last_four_hex_chars_of_id() {
        let identity = GuestIdentity::from_id("aaaaaaaa-bbbb-cccc-dddd-eeeeeeee1234".to_string());
        assert_eq!(identity.display_name, "Guest-1234");
    }

    #[test]
    fn cookie_roundtrip_preserves_identity() {
        let identity = GuestIdentity::new_random();
        let value = serde_json::to_string(&identity).unwrap();
        let roundtripped = GuestIdentity::from_cookie_value(&value).unwrap();
        assert_eq!(identity, roundtripped);
    }
}
