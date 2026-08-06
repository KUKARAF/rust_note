//! Share-token generation.
//!
//! A share token is a cryptographically random, URL-safe, unguessable
//! string that is the *sole* credential for a share link (see
//! `server::share` — `GET /api/shared/{token}`, `GET /ws/shared/{token}`):
//! anyone who has it can access the linked note per the permission recorded
//! alongside it, with no login. It must therefore be infeasible to guess or
//! enumerate.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;

/// Bytes of randomness per token: 18 bytes = 144 bits of entropy, comfortably
/// above the ~128-bit floor conventionally used for unguessable tokens (e.g.
/// UUIDv4's 122 bits of randomness).
const TOKEN_BYTES: usize = 18;

/// Generate a new, cryptographically-random, URL-safe share token.
///
/// Backed by `rand`'s OS-seeded thread-local CSPRNG, base64url-encoded
/// without padding so the result drops straight into a URL path segment
/// (`/shared/{token}`) with no escaping needed. 18 bytes encodes to exactly
/// 24 characters with no padding (18 is a multiple of 3).
pub fn generate() -> String {
    let bytes: [u8; TOKEN_BYTES] = rand::random();
    URL_SAFE_NO_PAD.encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_url_safe_and_unpadded() {
        let token = generate();
        assert!(!token.is_empty());
        assert!(
            token
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
            "token {token:?} contains a character unsafe for a URL path segment"
        );
        assert!(!token.contains('='), "no-pad encoding must not add '='");
    }

    #[test]
    fn tokens_have_144_bits_of_entropy() {
        // 18 raw bytes -> 24 base64url chars, no padding.
        let token = generate();
        assert_eq!(token.len(), 24);
    }

    #[test]
    fn tokens_are_unique() {
        let tokens: std::collections::HashSet<String> = (0..2000).map(|_| generate()).collect();
        assert_eq!(
            tokens.len(),
            2000,
            "2000 generated tokens must all be distinct"
        );
    }
}
