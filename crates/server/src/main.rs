mod auth;
mod collab;
mod config;
mod db;
mod db_users;
mod error;
mod notes;
mod routes;
mod settings;
mod share;
mod state;
mod todos;

use std::sync::Arc;

use auth::oidc::OidcClient;
use axum::http::{HeaderValue, Method};
use axum_extra::extract::cookie::Key;
use config::Config;
use notes::repo::NotesRepo;
use state::AppState;
use tower_http::cors::CorsLayer;
use tower_sessions::cookie::time::Duration as CookieDuration;
use tower_sessions::{Expiry, SessionManagerLayer};
use tower_sessions_sqlx_store::SqliteStore;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let config = Config::from_env();
    tracing::info!(bind_addr = %config.bind_addr, "starting rust_note server");

    // Fail fast on insecure/misconfigured deployments (AA-1: committed dev
    // signing key on a real deployment; AA-2: dev mode bound to a
    // non-loopback interface).
    config.validate()?;

    // Fail fast on a misconfigured signing key rather than limping along
    // with an unusable cookie key.
    // `derive_from` (as opposed to `Key::from`) only requires >= 32 bytes
    // of input entropy and derives the actual signing/encryption keys from
    // it via HKDF, which is what `Config::signing_key_bytes`'s minimum-
    // length check is calibrated against.
    let signing_key_bytes = config.signing_key_bytes()?;
    let cookie_key = Key::derive_from(&signing_key_bytes);

    let pool = db::init_pool(&config.sqlite_path).await?;
    let notes_repo = NotesRepo::open_or_init(&config.notes_repo_path)?;

    // Convert any legacy Obsidian excalidraw wrapper files to bare drawings
    // BEFORE anything can list notes or open rooms (no rooms exist yet at
    // this point, so there is nothing to race). Converges to a no-op.
    let migration = notes::excalidraw_migration::migrate(&notes_repo).await?;
    if !migration.is_noop() {
        tracing::info!(
            converted = migration.converted,
            broken_renamed = migration.broken_renamed,
            skipped_conflicts = migration.skipped_conflicts,
            "excalidraw wrapper migration ran"
        );
    }

    if config.dev_mode {
        tracing::warn!(
            "DEV MODE ENABLED: authentication is bypassed, every request is treated as user \
             \"{}\". Do not expose this server to anyone else while dev mode is on.",
            config::DEV_MODE_USER_ID
        );
        sqlx::query(
            "INSERT INTO users (id, email, display_name, created_at) \
             VALUES (?, ?, ?, datetime('now')) \
             ON CONFLICT(id) DO NOTHING",
        )
        .bind(config::DEV_MODE_USER_ID)
        .bind("admin@localhost")
        .bind("Admin")
        .execute(&pool)
        .await?;

        // Files that already exist on disk (e.g. an existing vimwiki
        // directory pointed at via RUSTNOTE_NOTES_REPO_PATH) were never
        // "created" through the app, so they have no `notes` row and are
        // invisible to the ACL-checked routes. In dev mode, auto-register
        // every note currently on disk as owned by the dev user so the
        // whole existing vault shows up immediately.
        let existing_paths = notes_repo.list_notes()?;
        for path in &existing_paths {
            let note_id = notes::fs_store::path_to_note_id(path);
            notes::acl::ensure_note_registered(&pool, &note_id, config::DEV_MODE_USER_ID).await?;
        }
        tracing::info!(
            count = existing_paths.len(),
            "dev mode: auto-registered existing notes on disk"
        );
    }

    // OIDC discovery talks to the (possibly not-yet-reachable, e.g. in a
    // sandbox or a docker-compose stack that hasn't finished starting
    // Authentik yet) issuer over the network. We do NOT want that to block
    // `main()` forever, nor do we want a transient failure to be fully
    // fatal - so: a few retries with backoff (inside `OidcClient::discover`),
    // and if it still fails, log it loudly and boot anyway with OIDC routes
    // returning a clear 500 (`AppState.oidc == None`) instead of panicking.
    // This keeps `/health` (and anything else that doesn't need auth) up
    // even when the identity provider is down.
    let oidc = if config.dev_mode {
        tracing::info!("dev mode: skipping OIDC discovery entirely");
        None
    } else {
        match OidcClient::discover(&config).await {
            Ok(client) => {
                tracing::info!("OIDC discovery succeeded");
                Some(Arc::new(client))
            }
            Err(err) => {
                tracing::error!(
                    error = %err,
                    "OIDC discovery failed at startup; /auth/login and /auth/callback will return 500 \
                     until this is fixed and the server is restarted"
                );
                None
            }
        }
    };

    let session_store = SqliteStore::new(pool.clone());
    session_store.migrate().await?;
    let session_layer = SessionManagerLayer::new(session_store)
        .with_name("rustnote_session")
        // `false` in dev so the cookie works over plain HTTP on localhost.
        // Production deployments MUST run behind HTTPS. The flag is derived
        // from the base URL by default, but `RUSTNOTE_COOKIE_SECURE` can
        // override that heuristic explicitly (SCD-3).
        .with_secure(config.cookie_secure())
        .with_same_site(tower_sessions::cookie::SameSite::Lax)
        .with_expiry(Expiry::OnInactivity(CookieDuration::days(14)));

    // The SvelteKit dev server (Vite, port 5173) and this backend run on
    // different origins in local dev (and the Tauri webview will too, in
    // production) - the browser's fetch spec requires the server to
    // explicitly opt each origin into cross-origin requests, including
    // preflighted ones, or the browser refuses to even hand the response to
    // JS (surfaces as an opaque "network error", not a visible HTTP status).
    // `allow_credentials(true)` is required since auth is cookie-based, and
    // per the fetch spec that means we can NOT use a wildcard origin - each
    // allowed origin must be listed explicitly, which is what
    // `config.cors_allowed_origins` is for.
    let cors_origins: Vec<HeaderValue> = config
        .cors_allowed_origins
        .iter()
        .filter_map(|o| HeaderValue::from_str(o).ok())
        .collect();
    let cors = CorsLayer::new()
        .allow_origin(cors_origins)
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::PATCH,
            Method::DELETE,
            Method::OPTIONS,
        ])
        // NOTE: `allow_credentials(true)` can never be paired with a
        // wildcard (`Any`) origin/header/method per the CORS spec -
        // tower-http enforces this at request time (panics), not just at
        // build time, so it's easy to reintroduce this bug by accident.
        // `mirror_request()` reflects back whatever headers the browser's
        // preflight actually asked for, which satisfies the spec without
        // hardcoding a header allowlist that'll go stale.
        .allow_headers(tower_http::cors::AllowHeaders::mirror_request())
        .allow_credentials(true);

    let state = AppState {
        db: pool,
        notes_repo,
        config: Arc::new(config.clone()),
        oidc,
        cookie_key,
        note_locks: crate::state::NoteLocks::new(),
        rooms: crate::collab::room::RoomRegistry::new(),
    };

    // Kept for the graceful-shutdown flush of live collab rooms below.
    let shutdown_state = state.clone();

    let app = routes::build(state).layer(session_layer).layer(cors);

    let listener = tokio::net::TcpListener::bind(&config.bind_addr).await?;
    tracing::info!("listening on {}", listener.local_addr()?);
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal(shutdown_state))
        .await?;

    Ok(())
}

/// Resolves when the process receives Ctrl-C or (on Unix) SIGTERM, then
/// force-flushes every live collaboration room so in-flight CRDT edits reach
/// git before we exit (redeploys send SIGTERM). Best-effort: individual flush
/// failures are logged inside `flush_all`, not propagated.
async fn shutdown_signal(state: AppState) {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sig) => {
                sig.recv().await;
            }
            Err(err) => {
                tracing::error!(error = %err, "failed to install SIGTERM handler");
                std::future::pending::<()>().await;
            }
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }

    tracing::info!("shutdown signal received; flushing live collab rooms");
    state.rooms.flush_all(&state).await;
}
