//! Application configuration, loaded from environment variables.
//!
//! All variables are prefixed `RUSTNOTE_`. Every field has a dev-friendly
//! default so the server can be started locally with zero configuration;
//! production deployments are expected to override these via the
//! environment.

#[derive(Debug, Clone)]
pub struct Config {
    /// Base URL of the Authentik (or other OIDC provider) issuer.
    pub authentik_issuer_url: String,
    /// OIDC client id registered with the identity provider.
    pub oidc_client_id: String,
    /// OIDC client secret registered with the identity provider, if the
    /// client is registered as "Confidential". `None` (unset/empty env var)
    /// means the client is "Public" - authenticated via PKCE alone (RFC
    /// 7636), with no shared secret at all. Authentik supports both; use
    /// whichever the application's Provider is actually configured as.
    pub oidc_client_secret: Option<String>,
    /// Redirect URI the identity provider will send the browser back to.
    pub oidc_redirect_uri: String,
    /// Key used to sign session cookies.
    pub cookie_signing_key: String,
    /// Public base URL this server is reachable at.
    pub base_url: String,
    /// Filesystem path to the git-backed notes repository.
    pub notes_repo_path: String,
    /// Filesystem path to the sqlite database file.
    pub sqlite_path: String,
    /// Address (host:port) to bind the HTTP server to.
    pub bind_addr: String,
    /// Explicit override for the session cookie's `Secure` flag
    /// (`RUSTNOTE_COOKIE_SECURE=true|false`). When `None`, the flag is derived
    /// from whether [`Self::base_url`] is `https://` (see [`Self::cookie_secure`]).
    pub cookie_secure: Option<bool>,
    /// If true, all authentication is bypassed and every request is treated
    /// as the fixed user "admin" - no OIDC discovery, no session cookie, no
    /// login flow. Enabled by setting `RUSTNOTE_ENV=dev` (or
    /// `RUSTNOTE_DEV_MODE=true` directly). **Never enable this in a
    /// deployment reachable by anyone other than you.**
    pub dev_mode: bool,
    /// Origins allowed to make cross-origin requests to this API (the
    /// SvelteKit dev server, and eventually the Tauri webview, run on a
    /// different origin than this backend). Comma-separated via
    /// `RUSTNOTE_CORS_ORIGINS`; defaults cover the standard Vite dev server
    /// ports on both `localhost` and `127.0.0.1` (browsers treat these as
    /// distinct origins) plus Tauri's default dev port.
    pub cors_allowed_origins: Vec<String>,
    /// Optional filesystem path to the built SvelteKit static assets
    /// (`web/build`, i.e. adapter-static's output). When set, the server
    /// serves them directly (SPA fallback to `200.html` for any non-API
    /// route), so the frontend and backend can be deployed as a single
    /// origin/container behind a reverse proxy with no CORS needed. Unset in
    /// local dev, where the Vite dev server serves the frontend separately.
    pub static_dir: Option<String>,
}

/// User id used for every request when [`Config::dev_mode`] is enabled.
pub const DEV_MODE_USER_ID: &str = "admin";

/// The committed, INSECURE default cookie signing key baked into
/// [`Config::from_env`] so local dev works with zero setup. It satisfies the
/// length check, so a real deployment that forgets to set
/// `RUSTNOTE_COOKIE_SIGNING_KEY` would otherwise boot silently on a key that
/// is public knowledge (AA-1). [`Config::validate`] refuses to boot on this
/// value unless dev mode is on.
pub const DEV_DEFAULT_COOKIE_SIGNING_KEY: &str =
    "ZGV2LWluc2VjdXJlLXNpZ25pbmcta2V5LWNoYW5nZS1tZS0hISEh";

/// The dev-friendly default [`Config::base_url`]. Fine for local dev (no one
/// else can reach it); a real deployment must set `RUSTNOTE_BASE_URL`
/// explicitly (to its real public URL, e.g. `https://notes.example.com`) -
/// this value is also the OIDC redirect target and drives the session
/// cookie's `Secure` flag heuristic, so silently booting on `localhost`
/// would send a real deployment's login flow and cookies to the wrong place.
/// [`Config::validate`] refuses to boot on this value unless dev mode is on.
pub const DEV_DEFAULT_BASE_URL: &str = "http://localhost:8080";

/// Minimum length (in bytes, after decoding) required of the cookie signing
/// key. `cookie::Key::derive_from` / `axum_extra`'s signed/private cookie
/// jars need at least 32 bytes of entropy (they use it to derive both a
/// signing and an encryption key internally), but we require a little
/// headroom above that floor.
pub const MIN_COOKIE_SIGNING_KEY_BYTES: usize = 32;

impl Config {
    /// Load configuration from the environment, falling back to
    /// development-friendly defaults for anything unset.
    pub fn from_env() -> Self {
        Self {
            authentik_issuer_url: std::env::var("RUSTNOTE_AUTHENTIK_ISSUER_URL")
                .unwrap_or_else(|_| "http://localhost:9000/application/o/rust-note/".to_string()),
            oidc_client_id: std::env::var("RUSTNOTE_OIDC_CLIENT_ID")
                .unwrap_or_else(|_| "rust-note-dev".to_string()),
            oidc_client_secret: std::env::var("RUSTNOTE_OIDC_CLIENT_SECRET")
                .ok()
                .filter(|s| !s.is_empty()),
            oidc_redirect_uri: std::env::var("RUSTNOTE_OIDC_REDIRECT_URI")
                .unwrap_or_else(|_| "http://localhost:8080/auth/callback".to_string()),
            cookie_signing_key: std::env::var("RUSTNOTE_COOKIE_SIGNING_KEY").unwrap_or_else(|_| {
                // 44 base64 chars == 32 raw bytes, i.e. exactly the dev
                // default needed to satisfy `signing_key_bytes` below
                // with zero setup. Production deployments MUST override
                // this via the environment; `validate()` refuses to boot on
                // this value outside dev mode (AA-1).
                DEV_DEFAULT_COOKIE_SIGNING_KEY.to_string()
            }),
            base_url: std::env::var("RUSTNOTE_BASE_URL")
                .unwrap_or_else(|_| DEV_DEFAULT_BASE_URL.to_string()),
            notes_repo_path: std::env::var("RUSTNOTE_NOTES_REPO_PATH")
                .unwrap_or_else(|_| "./data/notes".to_string()),
            sqlite_path: std::env::var("RUSTNOTE_SQLITE_PATH")
                .unwrap_or_else(|_| "./data/rust_note.db".to_string()),
            bind_addr: std::env::var("RUSTNOTE_BIND_ADDR")
                // AA-2: default to loopback, NOT 0.0.0.0. Dev mode bypasses
                // auth entirely, so a 0.0.0.0 default would expose an
                // unauthenticated admin surface to the whole LAN out of the
                // box. A real deployment (with auth) can still opt into
                // 0.0.0.0 by setting this explicitly with dev mode off.
                .unwrap_or_else(|_| "127.0.0.1:8080".to_string()),
            cookie_secure: std::env::var("RUSTNOTE_COOKIE_SECURE").ok().and_then(|v| {
                match v.trim().to_ascii_lowercase().as_str() {
                    "true" | "1" | "yes" => Some(true),
                    "false" | "0" | "no" => Some(false),
                    // An unrecognized value falls back to the heuristic rather
                    // than guessing.
                    _ => None,
                }
            }),
            dev_mode: std::env::var("RUSTNOTE_ENV").as_deref() == Ok("dev")
                || std::env::var("RUSTNOTE_DEV_MODE").as_deref() == Ok("true"),
            cors_allowed_origins: std::env::var("RUSTNOTE_CORS_ORIGINS")
                .map(|v| v.split(',').map(|s| s.trim().to_string()).collect())
                .unwrap_or_else(|_| {
                    [
                        "http://localhost:5173",
                        "http://127.0.0.1:5173",
                        "http://localhost:1420",
                        "http://127.0.0.1:1420",
                        // The Tauri Android app's webview origin — fixed (no
                        // port roulette) in both dev and release builds.
                        // Browsers cannot forge an Origin header, and anything
                        // that *can* claim this origin is already running on
                        // the user's own device, so allowing it by default is
                        // safe. NOTE: setting RUSTNOTE_CORS_ORIGINS *replaces*
                        // this whole list — deployments overriding it must
                        // include http://tauri.localhost for the app to work.
                        "http://tauri.localhost",
                    ]
                    .into_iter()
                    .map(String::from)
                    .collect()
                }),
            static_dir: std::env::var("RUSTNOTE_STATIC_DIR").ok(),
        }
    }

    /// The effective `Secure` flag for the session cookie: the explicit
    /// `RUSTNOTE_COOKIE_SECURE` override if set, otherwise the historical
    /// heuristic of "secure iff the public base URL is https" (SCD-3).
    pub fn cookie_secure(&self) -> bool {
        self.cookie_secure
            .unwrap_or_else(|| self.base_url.starts_with("https://"))
    }

    /// Whether [`Self::bind_addr`]'s host resolves to a loopback interface
    /// (`127.0.0.0/8`, `::1`, or the literal `localhost`).
    pub fn bind_is_loopback(&self) -> bool {
        use std::net::{IpAddr, SocketAddr};

        if let Ok(addr) = self.bind_addr.parse::<SocketAddr>() {
            return addr.ip().is_loopback();
        }
        // Non-numeric host (e.g. "localhost:8080"): split off the port and
        // inspect the host part.
        let host = match self.bind_addr.strip_prefix('[') {
            // Bracketed IPv6 literal without a resolvable SocketAddr above.
            Some(rest) => rest.split(']').next().unwrap_or(rest),
            None => self
                .bind_addr
                .rsplit_once(':')
                .map(|(h, _)| h)
                .unwrap_or(self.bind_addr.as_str()),
        };
        if let Ok(ip) = host.parse::<IpAddr>() {
            return ip.is_loopback();
        }
        host.eq_ignore_ascii_case("localhost")
    }

    /// Validate cross-field invariants that must hold before the server is
    /// allowed to boot. Returns an error (fail-fast) for misconfigurations
    /// that would otherwise silently expose the deployment.
    pub fn validate(&self) -> anyhow::Result<()> {
        // AA-1: never boot a real deployment on the committed, public dev
        // signing key.
        if self.cookie_signing_key.trim() == DEV_DEFAULT_COOKIE_SIGNING_KEY {
            if self.dev_mode {
                tracing::warn!(
                    "using the built-in INSECURE dev cookie signing key; set \
                     RUSTNOTE_COOKIE_SIGNING_KEY to a real secret before exposing this server"
                );
            } else {
                anyhow::bail!(
                    "refusing to boot: RUSTNOTE_COOKIE_SIGNING_KEY is unset or left at the \
                     built-in insecure dev default. Set it to a real secret (>= {} bytes).",
                    MIN_COOKIE_SIGNING_KEY_BYTES
                );
            }
        }

        // AA-2: dev mode bypasses authentication entirely, so it must never
        // listen on a non-loopback interface.
        if self.dev_mode && !self.bind_is_loopback() {
            anyhow::bail!(
                "refusing to boot: dev mode bypasses authentication and must not bind a \
                 non-loopback address (bind_addr = {}). Bind a loopback address (e.g. \
                 127.0.0.1:8080) or disable dev mode.",
                self.bind_addr
            );
        }

        // A real deployment must explicitly set its own public URL. Booting
        // on the dev default would silently point the OIDC redirect and the
        // cookie-Secure-flag heuristic at `localhost`, which is wrong for
        // every real deployment and easy to miss (unlike the signing-key/
        // bind checks above, nothing about this fails loudly on its own -
        // OIDC discovery can still succeed, the server still "works", it
        // just redirects users to the wrong place).
        if !self.dev_mode && self.base_url == DEV_DEFAULT_BASE_URL {
            anyhow::bail!(
                "refusing to boot: RUSTNOTE_BASE_URL is unset or left at the dev default ({}). \
                 Set it to this deployment's real public URL (e.g. https://notes.example.com).",
                DEV_DEFAULT_BASE_URL
            );
        }

        Ok(())
    }

    /// Decode [`Self::cookie_signing_key`] to raw bytes suitable for signing
    /// cookies, validating that it is long enough.
    ///
    /// The key is expected to be standard base64 (padded). If it isn't valid
    /// base64 we fall back to using its raw UTF-8 bytes directly (still
    /// subject to the same minimum-length check) so that a simple long
    /// random string works too, not just base64.
    pub fn signing_key_bytes(&self) -> anyhow::Result<Vec<u8>> {
        use base64::Engine;

        let bytes = base64::engine::general_purpose::STANDARD
            .decode(self.cookie_signing_key.trim())
            .unwrap_or_else(|_| self.cookie_signing_key.as_bytes().to_vec());

        anyhow::ensure!(
            bytes.len() >= MIN_COOKIE_SIGNING_KEY_BYTES,
            "RUSTNOTE_COOKIE_SIGNING_KEY must decode (or, if not valid base64, be) to at least {} bytes, got {}",
            MIN_COOKIE_SIGNING_KEY_BYTES,
            bytes.len()
        );

        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dev_default_signing_key_is_valid() {
        let config = Config::from_env();
        let bytes = config
            .signing_key_bytes()
            .expect("dev default should be valid");
        assert!(bytes.len() >= MIN_COOKIE_SIGNING_KEY_BYTES);
    }

    #[test]
    fn short_signing_key_is_rejected() {
        let mut config = Config::from_env();
        config.cookie_signing_key = "too-short".to_string();
        assert!(config.signing_key_bytes().is_err());
    }

    #[test]
    fn long_raw_signing_key_is_accepted_even_if_not_base64() {
        let mut config = Config::from_env();
        config.cookie_signing_key = "x".repeat(64);
        assert!(config.signing_key_bytes().is_ok());
    }

    #[test]
    fn dev_default_key_refuses_to_boot_outside_dev_mode() {
        // AA-1: the committed dev default key must not silently boot a real
        // (non-dev) deployment.
        let mut config = Config::from_env();
        config.cookie_signing_key = DEV_DEFAULT_COOKIE_SIGNING_KEY.to_string();
        config.dev_mode = false;
        config.bind_addr = "0.0.0.0:8080".to_string();
        assert!(
            config.validate().is_err(),
            "dev default key must be rejected when dev_mode is off"
        );

        // A real secret + a real base_url boots fine.
        config.cookie_signing_key = "x".repeat(64);
        config.base_url = "https://notes.example.com".to_string();
        assert!(config.validate().is_ok());
    }

    #[test]
    fn dev_default_key_allowed_in_dev_mode_on_loopback() {
        let mut config = Config::from_env();
        config.cookie_signing_key = DEV_DEFAULT_COOKIE_SIGNING_KEY.to_string();
        config.dev_mode = true;
        config.bind_addr = "127.0.0.1:8080".to_string();
        assert!(config.validate().is_ok());
    }

    #[test]
    fn dev_default_base_url_refuses_to_boot_outside_dev_mode() {
        let mut config = Config::from_env();
        config.cookie_signing_key = "x".repeat(64); // isolate the base_url check
        config.dev_mode = false;

        config.base_url = DEV_DEFAULT_BASE_URL.to_string();
        assert!(
            config.validate().is_err(),
            "dev default base_url must be rejected when dev_mode is off"
        );

        config.base_url = "https://notes.example.com".to_string();
        assert!(
            config.validate().is_ok(),
            "an explicit real base_url boots fine"
        );
    }

    #[test]
    fn dev_default_base_url_allowed_in_dev_mode() {
        let mut config = Config::from_env();
        config.dev_mode = true;
        config.bind_addr = "127.0.0.1:8080".to_string();
        // from_env()'s own default is DEV_DEFAULT_BASE_URL; dev mode must
        // boot with zero configuration.
        assert_eq!(config.base_url, DEV_DEFAULT_BASE_URL);
        assert!(config.validate().is_ok());
    }

    #[test]
    fn dev_mode_refuses_non_loopback_bind() {
        // AA-2: dev mode (no auth) must never bind a non-loopback address.
        let mut config = Config::from_env();
        config.cookie_signing_key = "x".repeat(64); // real key, isolate the bind check
        config.dev_mode = true;

        config.bind_addr = "0.0.0.0:8080".to_string();
        assert!(
            config.validate().is_err(),
            "dev mode + 0.0.0.0 must be refused"
        );

        config.bind_addr = "192.168.1.5:8080".to_string();
        assert!(
            config.validate().is_err(),
            "dev mode + LAN ip must be refused"
        );

        // Loopback variants are fine.
        for ok in ["127.0.0.1:8080", "localhost:8080", "[::1]:8080"] {
            config.bind_addr = ok.to_string();
            assert!(config.validate().is_ok(), "dev mode + {ok} must be allowed");
        }
    }

    #[test]
    fn bind_is_loopback_classifies_hosts() {
        let mut config = Config::from_env();
        for (addr, expected) in [
            ("127.0.0.1:8080", true),
            ("127.0.0.5:8080", true),
            ("localhost:8080", true),
            ("LocalHost:8080", true),
            ("[::1]:8080", true),
            ("0.0.0.0:8080", false),
            ("192.168.0.10:8080", false),
        ] {
            config.bind_addr = addr.to_string();
            assert_eq!(config.bind_is_loopback(), expected, "for {addr}");
        }
    }

    #[test]
    fn cookie_secure_override_wins_over_base_url_heuristic() {
        let mut config = Config::from_env();

        // No override: falls back to the base_url heuristic.
        config.cookie_secure = None;
        config.base_url = "https://notes.example.com".to_string();
        assert!(config.cookie_secure());
        config.base_url = "http://localhost:8080".to_string();
        assert!(!config.cookie_secure());

        // Explicit override wins in both directions.
        config.cookie_secure = Some(true);
        config.base_url = "http://localhost:8080".to_string();
        assert!(config.cookie_secure());
        config.cookie_secure = Some(false);
        config.base_url = "https://notes.example.com".to_string();
        assert!(!config.cookie_secure());
    }
}
