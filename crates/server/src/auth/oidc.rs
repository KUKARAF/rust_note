//! OIDC (Authentik) login flow: authorization redirect + callback handling.
//!
//! Discovery happens once at startup (see [`OidcClient::discover`], invoked
//! from `main.rs`) and the resulting client is stored in `AppState` behind
//! an `Option` — see the module-level notes on `main.rs` for why discovery
//! failure is treated as non-fatal.

use std::time::Duration;

use axum::extract::{Query, State};
use axum::response::Redirect;
use axum::routing::{get, post};
use axum::{Json, Router};
use axum_extra::extract::cookie::{Cookie, PrivateCookieJar};
use openidconnect::core::{CoreAuthenticationFlow, CoreClient, CoreProviderMetadata};
use openidconnect::{
    AuthorizationCode, ClientId, ClientSecret, CsrfToken, EndpointMaybeSet, EndpointNotSet,
    EndpointSet, IssuerUrl, Nonce, OAuth2TokenResponse, PkceCodeChallenge, PkceCodeVerifier,
    RedirectUrl, Scope, TokenResponse,
};
use serde::{Deserialize, Serialize};

use crate::auth::session;
use crate::config::Config;
use crate::error::{AppError, AppResult};
use crate::state::AppState;

/// The concrete `CoreClient` type once built from discovered provider
/// metadata. `from_provider_metadata` always sets the authorization
/// endpoint (`EndpointSet`) and *may* set the token/userinfo endpoints
/// depending on what the provider's discovery document contains
/// (`EndpointMaybeSet`); device-auth/introspection/revocation are never set
/// by discovery (`EndpointNotSet`). Getting these type parameters right is
/// what `openidconnect`/`oauth2`'s endpoint typestate requires for the
/// client to type-check — see their crate docs for details.
pub type ConfiguredCoreClient = CoreClient<
    EndpointSet,
    EndpointNotSet,
    EndpointNotSet,
    EndpointNotSet,
    EndpointMaybeSet,
    EndpointMaybeSet,
>;

/// A ready-to-use OIDC client plus the async HTTP client it talks through.
pub struct OidcClient {
    pub client: ConfiguredCoreClient,
    pub http_client: openidconnect::reqwest::Client,
}

impl OidcClient {
    /// Perform OIDC discovery against `config.authentik_issuer_url` and
    /// build a configured `CoreClient`.
    ///
    /// Discovery is retried a handful of times with backoff (Authentik may
    /// simply not be up yet when this server starts, e.g. in a fresh
    /// docker-compose stack) before giving up.
    pub async fn discover(config: &Config) -> anyhow::Result<Self> {
        let http_client = openidconnect::reqwest::ClientBuilder::new()
            // Following redirects on the *discovery* / token-exchange
            // requests opens the door to SSRF; we never want that here.
            .redirect(openidconnect::reqwest::redirect::Policy::none())
            .build()?;

        let issuer_url = IssuerUrl::new(config.authentik_issuer_url.clone())?;

        const MAX_ATTEMPTS: u32 = 3;
        let mut last_err = None;
        for attempt in 1..=MAX_ATTEMPTS {
            match CoreProviderMetadata::discover_async(issuer_url.clone(), &http_client).await {
                Ok(metadata) => {
                    // `None` here means a "Public" OIDC client (PKCE-only,
                    // RFC 7636, no shared secret) - the authorize/callback
                    // handlers below already always generate and verify a
                    // PKCE challenge regardless of client type, so a
                    // Confidential client (`Some`) just adds a second,
                    // belt-and-suspenders authentication factor on top.
                    let client_secret = config.oidc_client_secret.clone().map(ClientSecret::new);
                    let client = CoreClient::from_provider_metadata(
                        metadata,
                        ClientId::new(config.oidc_client_id.clone()),
                        client_secret,
                    )
                    .set_redirect_uri(RedirectUrl::new(config.oidc_redirect_uri.clone())?);

                    return Ok(Self {
                        client,
                        http_client,
                    });
                }
                Err(err) => {
                    tracing::warn!(
                        attempt,
                        max_attempts = MAX_ATTEMPTS,
                        error = %err,
                        issuer = %config.authentik_issuer_url,
                        "OIDC discovery attempt failed"
                    );
                    last_err = Some(err);
                    if attempt < MAX_ATTEMPTS {
                        tokio::time::sleep(Duration::from_secs(2u64.pow(attempt - 1))).await;
                    }
                }
            }
        }

        // The loop above always sets `last_err` before falling through, but
        // report a sensible error instead of unwrapping/panicking if that
        // invariant is ever violated by a future edit.
        Err(match last_err {
            Some(err) => anyhow::anyhow!(
                "OIDC discovery against {} failed after {MAX_ATTEMPTS} attempts: {}",
                config.authentik_issuer_url,
                err
            ),
            None => anyhow::anyhow!(
                "OIDC discovery against {} failed after {MAX_ATTEMPTS} attempts \
                 (no error was captured - this indicates a bug in the retry loop)",
                config.authentik_issuer_url
            ),
        })
    }
}

/// Name of the short-lived cookie holding PKCE verifier / CSRF state /
/// nonce between `/auth/login` and `/auth/callback`.
const FLOW_COOKIE_NAME: &str = "rustnote_oidc_flow";
const FLOW_COOKIE_MAX_AGE: time::Duration = time::Duration::minutes(5);

#[derive(Debug, Serialize, Deserialize)]
struct FlowState {
    pkce_verifier: String,
    csrf_state: String,
    nonce: String,
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/auth/login", get(login))
        .route("/auth/callback", get(callback))
        .route("/auth/logout", post(logout))
        .route("/auth/me", get(me))
}

async fn login(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
) -> AppResult<impl axum::response::IntoResponse> {
    let Some(oidc) = state.oidc.as_ref() else {
        return Err(AppError::Internal(anyhow::anyhow!(
            "OIDC is not configured (discovery failed or has not completed yet)"
        )));
    };

    let (pkce_challenge, pkce_verifier) = PkceCodeChallenge::new_random_sha256();

    let (authorize_url, csrf_state, nonce) = oidc
        .client
        .authorize_url(
            CoreAuthenticationFlow::AuthorizationCode,
            CsrfToken::new_random,
            Nonce::new_random,
        )
        .add_scope(Scope::new("openid".to_string()))
        .add_scope(Scope::new("profile".to_string()))
        .add_scope(Scope::new("email".to_string()))
        .set_pkce_challenge(pkce_challenge)
        .url();

    let flow_state = FlowState {
        pkce_verifier: pkce_verifier.secret().clone(),
        csrf_state: csrf_state.secret().clone(),
        nonce: nonce.secret().clone(),
    };
    let value = serde_json::to_string(&flow_state).map_err(|e| AppError::Internal(e.into()))?;

    let mut cookie = Cookie::new(FLOW_COOKIE_NAME, value);
    cookie.set_path("/auth");
    cookie.set_http_only(true);
    cookie.set_same_site(axum_extra::extract::cookie::SameSite::Lax);
    cookie.set_max_age(FLOW_COOKIE_MAX_AGE);
    let jar = jar.add(cookie);

    Ok((jar, Redirect::to(authorize_url.as_str())))
}

#[derive(Debug, Deserialize)]
struct CallbackParams {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

async fn callback(
    State(state): State<AppState>,
    Query(params): Query<CallbackParams>,
    jar: PrivateCookieJar,
    session: tower_sessions::Session,
) -> AppResult<impl axum::response::IntoResponse> {
    if let Some(err) = params.error {
        return Err(AppError::BadRequest(format!(
            "OIDC provider returned an error: {err} ({})",
            params.error_description.unwrap_or_default()
        )));
    }

    let Some(oidc) = state.oidc.as_ref() else {
        return Err(AppError::Internal(anyhow::anyhow!(
            "OIDC is not configured (discovery failed or has not completed yet)"
        )));
    };

    let code = params
        .code
        .ok_or_else(|| AppError::BadRequest("missing `code` query parameter".to_string()))?;
    let returned_state = params
        .state
        .ok_or_else(|| AppError::BadRequest("missing `state` query parameter".to_string()))?;

    let flow_cookie = jar
        .get(FLOW_COOKIE_NAME)
        .ok_or_else(|| AppError::BadRequest("missing or expired login flow cookie".to_string()))?;
    let flow_state: FlowState = serde_json::from_str(flow_cookie.value())
        .map_err(|_| AppError::BadRequest("invalid login flow cookie".to_string()))?;

    // Always clear the flow cookie regardless of outcome below - it's
    // single-use.
    let jar = jar.remove(Cookie::from(FLOW_COOKIE_NAME));

    if returned_state != flow_state.csrf_state {
        return Err(AppError::BadRequest("state mismatch".to_string()));
    }

    let token_response = oidc
        .client
        .exchange_code(AuthorizationCode::new(code))
        .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?
        .set_pkce_verifier(PkceCodeVerifier::new(flow_state.pkce_verifier))
        .request_async(&oidc.http_client)
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;

    let id_token = token_response.id_token().ok_or_else(|| {
        AppError::Internal(anyhow::anyhow!("provider did not return an ID token"))
    })?;
    let nonce = Nonce::new(flow_state.nonce);
    let id_token_verifier = oidc.client.id_token_verifier();
    let claims = id_token
        .claims(&id_token_verifier, &nonce)
        .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;

    let sub = claims.subject().as_str().to_string();
    let email = claims.email().map(|e| e.as_str().to_string());
    let display_name = claims
        .name()
        .and_then(|n| n.get(None))
        .map(|n| n.as_str().to_string())
        .or_else(|| claims.preferred_username().map(|u| u.as_str().to_string()));

    let now = time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .map_err(|e| AppError::Internal(e.into()))?;

    sqlx::query(
        "INSERT INTO users (id, email, display_name, created_at) VALUES (?, ?, ?, ?) \
         ON CONFLICT(id) DO UPDATE SET email = excluded.email, display_name = excluded.display_name",
    )
    .bind(&sub)
    .bind(&email)
    .bind(&display_name)
    .bind(&now)
    .execute(&state.db)
    .await
    .map_err(|e| AppError::Internal(e.into()))?;

    session::login(&session, &sub).await?;

    // `token_response` also carries an access token we don't currently use
    // for anything (we rely purely on session cookies going forward), but
    // we touch it here via the trait so we keep `OAuth2TokenResponse` in
    // scope for future use (e.g. calling the userinfo endpoint) without an
    // unused-import warning.
    let _ = token_response.access_token();

    // Redirect target after a successful login. `base_url` (root) is the
    // simplest sensible choice - the SPA is expected to live there and can
    // route itself to `/notes` etc. once it sees an authenticated session
    // via `GET /auth/me`.
    Ok((jar, Redirect::to(&state.config.base_url)))
}

async fn logout(session: tower_sessions::Session) -> AppResult<impl axum::response::IntoResponse> {
    session::logout(&session).await?;

    // NOTE: this does not redirect through Authentik's end-session endpoint
    // (RP-Initiated Logout), so the user's Authentik SSO session itself
    // stays alive - only the rust_note session is destroyed. Wiring up
    // full single-logout is a reasonable follow-up but isn't necessary for
    // Phase 1.
    Ok(Redirect::to("/"))
}

#[derive(Debug, Serialize)]
struct MeResponse {
    id: String,
    email: Option<String>,
    display_name: Option<String>,
}

async fn me(
    State(state): State<AppState>,
    session: tower_sessions::Session,
) -> AppResult<Json<MeResponse>> {
    let user_id = session::effective_user_id(&state, &session)
        .await
        .ok_or(AppError::Unauthorized)?;

    let row: Option<(String, Option<String>, Option<String>)> =
        sqlx::query_as("SELECT id, email, display_name FROM users WHERE id = ?")
            .bind(&user_id)
            .fetch_optional(&state.db)
            .await
            .map_err(|e| AppError::Internal(e.into()))?;

    let (id, email, display_name) = row.ok_or(AppError::Unauthorized)?;

    Ok(Json(MeResponse {
        id,
        email,
        display_name,
    }))
}
