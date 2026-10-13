//! Top-level route table.

use std::time::Duration;

use axum::extract::DefaultBodyLimit;
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;
use tower::limit::GlobalConcurrencyLimitLayer;
use tower_http::services::{ServeDir, ServeFile};
use tower_http::timeout::TimeoutLayer;

use crate::auth;
use crate::collab;
use crate::notes;
use crate::pipeline;
use crate::recurring;
use crate::settings;
use crate::share;
use crate::state::AppState;
use crate::stats;
use crate::todos;
use crate::tts;

/// Maximum accepted request body size. A note is plain markdown; 1 MiB is far
/// above any reasonable note yet cheaply rejects a body meant to exhaust
/// memory/disk with a clean 413 rather than relying on the implicit default
/// (dos-04).
// 4 MiB rather than 1 MiB: Excalidraw drawings with embedded images
// routinely exceed 1 MiB; 4 MiB still bounds abusive payloads.
const MAX_BODY_BYTES: usize = 4 * 1024 * 1024;

/// Per-request wall-clock timeout for the REST/API surface. Long enough for a
/// slow git commit under load, short enough to shed a stuck request (dos-03).
/// The WebSocket route is deliberately NOT wrapped in this — a live collab
/// connection is long-lived and must not be severed at 30s.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Ceiling on REST requests processed concurrently, so a burst of expensive
/// handlers (each of which can contend on the single global repo mutex) can't
/// pile up unboundedly (dos-03). Generous — normal traffic never approaches
/// it; it only caps pathological concurrency. The WebSocket route is excluded
/// so long-lived connections don't consume these permits.
const MAX_CONCURRENT_REQUESTS: usize = 256;

/// Build the full application router: the health check, the `/auth/*`,
/// `/api/notes/*` and `/api/shares*`/`/api/shared/*` routes, and the
/// `/ws/notes/*` + `/ws/shared/*` collab WebSockets. The session layer is not
/// applied here — the caller in `main.rs` does that, so it can also wrap any
/// future routers mounted here.
pub fn build(state: AppState) -> Router {
    // REST/API surface: timeout + body limit + a global concurrency ceiling.
    let api = Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/api/version", get(version))
        .merge(auth::oidc::router())
        .merge(notes::routes::router())
        .merge(todos::routes::router())
        .merge(todos::query::router())
        .merge(pipeline::routes::router())
        .merge(settings::routes::router())
        .merge(stats::routes::router())
        .merge(recurring::routes::router())
        .merge(share::router())
        .layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            REQUEST_TIMEOUT,
        ))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .layer(GlobalConcurrencyLimitLayer::new(MAX_CONCURRENT_REQUESTS));

    // The collab WebSockets (`/ws/notes/*` and the guest `/ws/shared/*`) are
    // intentionally left OUT of the timeout / concurrency / body-limit layers
    // above: they are long-lived, streaming connections that a 30s request
    // timeout would kill and that shouldn't occupy a REST concurrency permit
    // for their whole lifetime.
    let ws = collab::ws::router();

    // TTS synthesis (`POST /api/tts`) is also mounted OUTSIDE the 30s REST
    // timeout layer: synthesizing a whole long note can take well over 30s and
    // the handler already uses a dedicated reqwest client with its own longer
    // (90s) timeout + an input-size cap, so it governs its own duration. Left
    // in the REST layer it was cut off at 30s and surfaced a spurious 408.
    let tts = tts::routes::router();

    let app = api.merge(ws).merge(tts).with_state(state.clone());

    // Mount the MCP Streamable-HTTP server (`/mcp`) + its OAuth metadata,
    // outside the REST timeout/concurrency/body layers (like `ws`): its POST/GET
    // streams are long-lived and it does its own body handling.
    let app = crate::mcp::mount(app, state.clone());

    // Optionally serve the built SvelteKit static assets (adapter-static's
    // `web/build`) so the frontend + backend can run as one origin/container
    // behind a reverse proxy - no CORS needed, no separate static host. Any
    // path not matched by an API/auth/ws route above falls through to
    // `ServeDir`, which itself falls back to `200.html` (the SPA shell) for
    // any path that isn't a real file on disk, so client-side routing works
    // for direct loads/refreshes of e.g. `/notes/foo`. In local dev
    // `static_dir` is unset and the Vite dev server serves the frontend
    // separately, so this is a no-op there.
    match state.config.static_dir.as_deref() {
        Some(dir) => {
            let spa_fallback = ServeFile::new(format!("{dir}/200.html"));
            let serve_dir = ServeDir::new(dir).not_found_service(spa_fallback);
            app.fallback_service(serve_dir)
        }
        None => app,
    }
}

/// Build/version info surfaced in the Settings footer so it's unambiguous which
/// image is running. The git sha is baked into the image at build time by the
/// Dockerfile (`ARG GIT_SHA` → `ENV RUSTNOTE_BUILD_SHA`), which the CI
/// `docker-publish` workflow fills from `github.sha`. Falls back to `"dev"` for
/// local runs where the env var is unset. Public (no auth) — it leaks nothing.
#[derive(Serialize)]
struct VersionInfo {
    sha: String,
    short_sha: String,
}

async fn version() -> Json<VersionInfo> {
    let sha = std::env::var("RUSTNOTE_BUILD_SHA").unwrap_or_else(|_| "dev".to_string());
    let short_sha = sha.chars().take(7).collect::<String>();
    Json(VersionInfo { sha, short_sha })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::oidc::OidcClient;
    use crate::config::Config;
    use crate::notes::repo::NotesRepo;
    use crate::state::AppState;
    use axum::body::Body;
    use axum::http::Request;
    use axum_extra::extract::cookie::Key;
    use std::sync::Arc;
    use tower::ServiceExt; // for `oneshot`

    async fn dev_state() -> (AppState, tempfile::TempDir, tempfile::TempDir) {
        let notes_dir = tempfile::tempdir().unwrap();
        let db_dir = tempfile::tempdir().unwrap();
        let db_path = db_dir.path().join("test.db");
        let notes_repo = NotesRepo::open_or_init(notes_dir.path().to_str().unwrap()).unwrap();
        let db = crate::db::init_pool(db_path.to_str().unwrap())
            .await
            .unwrap();
        // dev mode so `RequireAuth` passes without a session, letting the
        // request reach the JSON body extractor where the body limit bites.
        let mut config = Config::from_env();
        config.dev_mode = true;
        let state = AppState {
            db,
            notes_repo,
            config: Arc::new(config),
            oidc: None::<Arc<OidcClient>>,
            cookie_key: Key::generate(),
            note_locks: crate::state::NoteLocks::new(),
            rooms: crate::collab::room::RoomRegistry::new(),
        };
        (state, notes_dir, db_dir)
    }

    #[tokio::test]
    async fn oversized_request_body_is_rejected_with_413() {
        // dos-04 regression: a body over the 1 MiB limit must be rejected
        // with 413 rather than being buffered.
        let (state, _notes_dir, _db_dir) = dev_state().await;
        let app = build(state);

        let huge = "x".repeat(5 * 1024 * 1024); // 5 MiB, over the 4 MiB cap
        let body = format!("{{\"id_or_title\":\"big\",\"content\":\"{huge}\"}}");
        let req = Request::builder()
            .method("POST")
            .uri("/api/notes")
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }

    /// Non-dev-mode state plus a seeded user and device token, with the
    /// session layer applied (as `main.rs` does) so handlers taking a
    /// `Session` extractor work in oneshot tests.
    async fn bearer_app() -> (
        axum::Router,
        String, // raw device token for user "u1"
        tempfile::TempDir,
        tempfile::TempDir,
    ) {
        let (mut state, notes_dir, db_dir) = dev_state().await;
        {
            let config = Arc::get_mut(&mut state.config).expect("config not yet shared");
            config.dev_mode = false;
        }

        sqlx::query(
            "INSERT INTO users (id, email, display_name, created_at) \
             VALUES ('u1', 'u1@example.com', 'User One', '2026-01-01T00:00:00Z')",
        )
        .execute(&state.db)
        .await
        .unwrap();
        let token = crate::auth::device_token::create(&state.db, "u1", "android-app")
            .await
            .unwrap();

        let session_layer =
            tower_sessions::SessionManagerLayer::new(tower_sessions::MemoryStore::default());
        let app = build(state).layer(session_layer);
        (app, token, notes_dir, db_dir)
    }

    fn get_with_bearer(uri: &str, token: &str) -> Request<Body> {
        Request::builder()
            .method("GET")
            .uri(uri)
            .header("authorization", format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap()
    }

    #[tokio::test]
    async fn bearer_token_authenticates_api_and_me() {
        let (app, token, _notes_dir, _db_dir) = bearer_app().await;

        let resp = app
            .clone()
            .oneshot(get_with_bearer("/api/notes", &token))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let resp = app
            .oneshot(get_with_bearer("/auth/me", &token))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn invalid_bearer_is_401_without_session_fallback() {
        let (app, _token, _notes_dir, _db_dir) = bearer_app().await;

        let resp = app
            .clone()
            .oneshot(get_with_bearer("/api/notes", "not-a-real-token"))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

        // And no credentials at all is equally a 401.
        let req = Request::builder()
            .method("GET")
            .uri("/api/notes")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn logout_with_bearer_revokes_the_token() {
        let (app, token, _notes_dir, _db_dir) = bearer_app().await;

        let req = Request::builder()
            .method("POST")
            .uri("/auth/logout")
            .header("authorization", format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);

        // The token must be dead afterwards.
        let resp = app
            .oneshot(get_with_bearer("/api/notes", &token))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn normal_sized_body_is_accepted() {
        // A small body must still be accepted (the limit doesn't reject
        // ordinary requests).
        let (state, _notes_dir, _db_dir) = dev_state().await;
        let app = build(state);

        let body = r#"{"id_or_title":"small","content":"hello"}"#;
        let req = Request::builder()
            .method("POST")
            .uri("/api/notes")
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_ne!(
            resp.status(),
            StatusCode::PAYLOAD_TOO_LARGE,
            "a normal body must not be rejected as too large"
        );
    }

    /// End-to-end MCP OAuth round trip through the real HTTP router:
    /// register a client (no auth), authorize it (dev-mode session bypass
    /// stands in for an already-logged-in browser), consent, exchange the
    /// code for a token pair, then refresh - confirming the whole
    /// self-contained Authorization Server (register -> authorize ->
    /// decision -> token -> refresh) is wired together correctly, not just
    /// each piece in isolation.
    #[tokio::test]
    async fn mcp_oauth_full_round_trip() {
        use base64::engine::general_purpose::URL_SAFE_NO_PAD;
        use base64::Engine;
        use sha2::{Digest, Sha256};

        let (state, _notes_dir, _db_dir) = dev_state().await; // dev_mode = true
                                                              // `dev_state()` doesn't replicate `main()`'s dev-mode bootstrap,
                                                              // which normally seeds this row before serving any request -
                                                              // needed here since minting an auth code/token FK-references it.
        sqlx::query(
            "INSERT INTO users (id, email, display_name, created_at) \
             VALUES ('admin', 'admin@localhost', 'Admin', '2026-01-01T00:00:00Z')",
        )
        .execute(&state.db)
        .await
        .unwrap();
        let session_layer =
            tower_sessions::SessionManagerLayer::new(tower_sessions::MemoryStore::default());
        let app = build(state).layer(session_layer);

        // 1. Register - unauthenticated, anyone can do this.
        let redirect_uri = "https://example.com/cb";
        let register_req = Request::builder()
            .method("POST")
            .uri("/mcp/register")
            .header("content-type", "application/json")
            .body(Body::from(format!(
                r#"{{"redirect_uris":["{redirect_uri}"],"client_name":"test client"}}"#
            )))
            .unwrap();
        let resp = app.clone().oneshot(register_req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let register_json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let client_id = register_json["client_id"].as_str().unwrap().to_string();
        assert!(register_json.get("client_secret").is_none());

        // 2. Authorize - dev_mode makes `session::effective_user_id` resolve
        // the fixed "admin" user with no real cookie needed, standing in for
        // an already-logged-in browser.
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let authorize_uri = format!(
            "/mcp/authorize?{}",
            urlencoded_pairs(&[
                ("response_type", "code"),
                ("client_id", &client_id),
                ("redirect_uri", redirect_uri),
                ("code_challenge", &challenge),
                ("code_challenge_method", "S256"),
                ("state", "xyz"),
            ])
        );
        let authorize_req = Request::builder()
            .method("GET")
            .uri(&authorize_uri)
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(authorize_req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "consent page should render");
        let flow_cookie = resp
            .headers()
            .get("set-cookie")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(';').next())
            .expect("authorize must set the flow cookie")
            .to_string();

        // 3. Decide - allow, carrying the flow cookie back.
        let decision_req = Request::builder()
            .method("POST")
            .uri("/mcp/authorize/decision")
            .header("content-type", "application/x-www-form-urlencoded")
            .header("cookie", &flow_cookie)
            .body(Body::from("decision=allow"))
            .unwrap();
        let resp = app.clone().oneshot(decision_req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::SEE_OTHER);
        let location = resp
            .headers()
            .get("location")
            .and_then(|v| v.to_str().ok())
            .unwrap()
            .to_string();
        assert!(location.starts_with(redirect_uri));
        let code = reqwest::Url::parse(&location)
            .unwrap()
            .query_pairs()
            .find(|(k, _)| k == "code")
            .map(|(_, v)| v.into_owned())
            .expect("redirect must carry a code");

        // 4. Token exchange (authorization_code grant).
        let token_req = Request::builder()
            .method("POST")
            .uri("/mcp/token")
            .header("content-type", "application/x-www-form-urlencoded")
            .body(Body::from(urlencoded_pairs(&[
                ("grant_type", "authorization_code"),
                ("code", &code),
                ("redirect_uri", redirect_uri),
                ("client_id", &client_id),
                ("code_verifier", verifier),
            ])))
            .unwrap();
        let resp = app.clone().oneshot(token_req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let token_json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let access_token = token_json["access_token"].as_str().unwrap().to_string();
        let refresh_token = token_json["refresh_token"].as_str().unwrap().to_string();

        // The minted access token must actually be accepted by the
        // Resource-Server check guarding `/mcp` itself.
        let mcp_req = Request::builder()
            .method("GET")
            .uri("/mcp")
            .header("authorization", format!("Bearer {access_token}"))
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(mcp_req).await.unwrap();
        assert_ne!(
            resp.status(),
            StatusCode::UNAUTHORIZED,
            "a freshly minted access token must pass the Resource-Server check"
        );

        // 5. Refresh - old refresh token must be rotated out.
        let refresh_req = Request::builder()
            .method("POST")
            .uri("/mcp/token")
            .header("content-type", "application/x-www-form-urlencoded")
            .body(Body::from(format!(
                "grant_type=refresh_token&refresh_token={refresh_token}&client_id={client_id}"
            )))
            .unwrap();
        let resp = app.clone().oneshot(refresh_req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let replay_req = Request::builder()
            .method("POST")
            .uri("/mcp/token")
            .header("content-type", "application/x-www-form-urlencoded")
            .body(Body::from(format!(
                "grant_type=refresh_token&refresh_token={refresh_token}&client_id={client_id}"
            )))
            .unwrap();
        let resp = app.oneshot(replay_req).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::BAD_REQUEST,
            "a rotated-out refresh token must not work twice"
        );
    }

    /// Build a properly percent-encoded `application/x-www-form-urlencoded`
    /// string (used for both a query string and a form body - the two are
    /// the same encoding) without pulling in a new crate just for this test.
    fn urlencoded_pairs(pairs: &[(&str, &str)]) -> String {
        let mut url = reqwest::Url::parse("http://placeholder.invalid/").unwrap();
        {
            let mut qp = url.query_pairs_mut();
            for (k, v) in pairs {
                qp.append_pair(k, v);
            }
        }
        url.query().unwrap_or("").to_string()
    }
}
