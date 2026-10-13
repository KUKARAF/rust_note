//! HTTP handlers for rust_note's self-contained MCP OAuth Authorization
//! Server: RFC 7591 registration, the browser-facing authorize/consent
//! flow, the RFC 6749 token endpoint, and RFC 8414 AS metadata.
//!
//! Every handler in this file is deliberately **unauthenticated at the HTTP
//! layer** - that's inherent to what each one is (a client has no token yet
//! when registering, authorizing, or exchanging a code). The actual access
//! control happens one layer up: [`authorize`] requires an existing
//! rust_note session (the same one `/auth/login` already establishes) and
//! shows a mandatory consent screen before ever minting a code - see that
//! function's doc comment for why the consent step is load-bearing, not
//! decorative, given registration itself is open to anyone.

use axum::extract::{Query, RawQuery, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Json, Router};
use axum_extra::extract::cookie::{Cookie, PrivateCookieJar};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

use super::store;
use crate::auth::session;
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/mcp/register", post(register))
        .route("/mcp/authorize", get(authorize))
        .route("/mcp/authorize/decision", post(authorize_decision))
        .route("/mcp/token", post(token))
        .route(
            "/.well-known/oauth-authorization-server",
            get(authorization_server_metadata),
        )
}

/// Build an OAuth-shaped JSON error body (RFC 6749 §5.2 / RFC 7591 §3.2.2
/// use the same `{error, error_description}` shape), bypassing the app's
/// generic `AppError`/`{message}` convention - same precedent as
/// `mcp::auth::McpAuth::challenge`.
fn oauth_error(status: StatusCode, error: &str, description: &str) -> Response {
    (
        status,
        Json(json!({ "error": error, "error_description": description })),
    )
        .into_response()
}

fn server_error(context: &str, err: impl std::fmt::Display) -> Response {
    tracing::error!(error = %err, context, "mcp oauth: internal error");
    oauth_error(
        StatusCode::INTERNAL_SERVER_ERROR,
        "server_error",
        "internal error",
    )
}

// ---- shared redirect_uri validation ---------------------------------------

const MAX_REDIRECT_URIS: usize = 10;
const MAX_REDIRECT_URI_LEN: usize = 2000;
const MAX_CLIENT_NAME_LEN: usize = 200;

/// Whether `uri` is acceptable as a dynamically-registered client's
/// redirect_uri: `https://` unconditionally; `http://` only to a loopback
/// host (any port) per RFC 8252's native-app allowance; any other scheme
/// (a private custom scheme, e.g. `dev.rustnote.app://auth`) accepted at
/// face value, since the OS - not the network - is what routes it, unlike a
/// bare `http://` URL which would send a code over cleartext.
fn is_acceptable_redirect_uri(uri: &str) -> bool {
    let Ok(parsed) = reqwest::Url::parse(uri.trim()) else {
        return false;
    };
    match parsed.scheme() {
        "https" => true,
        "http" => {
            let Some(host) = parsed.host_str() else {
                return false;
            };
            // `Url::host_str` keeps an IPv6 literal's brackets; strip them
            // before comparing (mirrors `recurring::is_safe_recurring_url`).
            let host = host
                .strip_prefix('[')
                .and_then(|h| h.strip_suffix(']'))
                .unwrap_or(host);
            host == "127.0.0.1" || host == "::1"
        }
        _ => true,
    }
}

/// Validate a full `redirect_uris` list (RFC 7591 registration request):
/// non-empty, bounded in count/length, and every entry individually
/// acceptable per [`is_acceptable_redirect_uri`].
fn validate_redirect_uris(uris: &[String]) -> Result<(), &'static str> {
    if uris.is_empty() {
        return Err("redirect_uris must contain at least one URI");
    }
    if uris.len() > MAX_REDIRECT_URIS {
        return Err("too many redirect_uris");
    }
    for uri in uris {
        if uri.len() > MAX_REDIRECT_URI_LEN {
            return Err("redirect_uri is too long");
        }
        if !is_acceptable_redirect_uri(uri) {
            return Err(
                "redirect_uris must be https://, a loopback http://127.0.0.1 or http://[::1] \
                 URI (any port), or a private custom scheme",
            );
        }
    }
    Ok(())
}

// ---- POST /mcp/register (RFC 7591) ----------------------------------------

#[derive(Debug, Deserialize)]
struct RegisterRequest {
    redirect_uris: Vec<String>,
    #[serde(default)]
    client_name: Option<String>,
}

#[derive(Debug, Serialize)]
struct RegisterResponse {
    client_id: String,
    client_id_issued_at: i64,
    redirect_uris: Vec<String>,
    token_endpoint_auth_method: &'static str,
    grant_types: [&'static str; 2],
    response_types: [&'static str; 1],
    #[serde(skip_serializing_if = "Option::is_none")]
    client_name: Option<String>,
}

async fn register(State(state): State<AppState>, Json(body): Json<RegisterRequest>) -> Response {
    if let Err(msg) = validate_redirect_uris(&body.redirect_uris) {
        return oauth_error(StatusCode::BAD_REQUEST, "invalid_redirect_uri", msg);
    }

    let client_name = body
        .client_name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.chars().take(MAX_CLIENT_NAME_LEN).collect::<String>());

    match store::count_clients(&state.db).await {
        Ok(count) if count >= store::MAX_MCP_CLIENTS => {
            return oauth_error(
                StatusCode::TOO_MANY_REQUESTS,
                "temporarily_unavailable",
                "too many registered MCP clients; contact the server operator",
            );
        }
        Ok(_) => {}
        Err(err) => return server_error("count_clients", err),
    }

    match store::register_client(&state.db, &body.redirect_uris, client_name.as_deref()).await {
        Ok(client_id) => Json(RegisterResponse {
            client_id,
            client_id_issued_at: OffsetDateTime::now_utc().unix_timestamp(),
            redirect_uris: body.redirect_uris,
            token_endpoint_auth_method: "none",
            grant_types: ["authorization_code", "refresh_token"],
            response_types: ["code"],
            client_name,
        })
        .into_response(),
        Err(err) => server_error("register_client", err),
    }
}

// ---- GET /mcp/authorize + POST /mcp/authorize/decision ---------------------

const AUTHORIZE_FLOW_COOKIE_NAME: &str = "rustnote_mcp_authorize_flow";
const AUTHORIZE_FLOW_COOKIE_MAX_AGE: time::Duration = time::Duration::minutes(5);

#[derive(Debug, Deserialize)]
struct AuthorizeParams {
    response_type: String,
    client_id: String,
    redirect_uri: String,
    code_challenge: String,
    code_challenge_method: String,
    state: Option<String>,
}

/// The validated, consent-pending request, stashed in a short-lived
/// signed/private cookie between the `GET` (render consent) and `POST`
/// (decide) steps - mirrors `auth::oidc`'s own login-flow cookie exactly.
/// This cookie, not the resubmitted form, is the only trusted source for
/// `client_id`/`redirect_uri`/`code_challenge`/`state` at decision time.
#[derive(Debug, Serialize, Deserialize)]
struct AuthorizeFlowState {
    client_id: String,
    redirect_uri: String,
    code_challenge: String,
    state: Option<String>,
    user_id: String,
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn bad_request_page(message: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Html(format!(
            "<!doctype html><html><body><p>{}</p></body></html>",
            html_escape(message)
        )),
    )
        .into_response()
}

/// The browser-facing authorization endpoint.
///
/// An unregistered/mismatched `client_id`/`redirect_uri` is rejected
/// directly here with a 400 page - **never** redirected to - per OAuth
/// security best practice (redirecting on an unvalidated redirect_uri is
/// itself the vulnerability).
///
/// Once the request is validated, a session is required (redirecting to the
/// existing `/auth/login?next=...` if absent) and a consent screen is
/// shown. The consent step is not optional: because [`super::register`] is
/// open to anyone, an attacker can register a client and lure an
/// already-logged-in victim straight to this URL. Without a screen naming
/// the client, that would silently mint the attacker a token for the
/// victim's account. The screen is the mitigation precisely because
/// registration itself has no gate.
async fn authorize(
    State(state): State<AppState>,
    Query(params): Query<AuthorizeParams>,
    RawQuery(raw_query): RawQuery,
    session: tower_sessions::Session,
    jar: PrivateCookieJar,
) -> Response {
    if params.response_type != "code" {
        return bad_request_page("unsupported response_type (only \"code\" is supported)");
    }
    if params.code_challenge_method != "S256" {
        return bad_request_page("unsupported code_challenge_method (only \"S256\" is supported)");
    }

    let client = match store::lookup_client(&state.db, &params.client_id).await {
        Ok(Some(client)) => client,
        Ok(None) => return bad_request_page("unknown client_id"),
        Err(err) => return server_error("lookup_client", err),
    };
    if !client.redirect_uris.contains(&params.redirect_uri) {
        return bad_request_page("redirect_uri does not match this client's registration");
    }

    let Some(user_id) = session::effective_user_id(&state, &session).await else {
        let next = format!("/mcp/authorize?{}", raw_query.unwrap_or_default());
        let login_url = format!("/auth/login?next={}", urlencoding_encode(&next));
        return Redirect::to(&login_url).into_response();
    };

    let flow = AuthorizeFlowState {
        client_id: params.client_id,
        redirect_uri: params.redirect_uri.clone(),
        code_challenge: params.code_challenge,
        state: params.state,
        user_id: user_id.clone(),
    };
    let value = match serde_json::to_string(&flow) {
        Ok(v) => v,
        Err(err) => return server_error("serialize authorize flow", err),
    };
    let mut cookie = Cookie::new(AUTHORIZE_FLOW_COOKIE_NAME, value);
    cookie.set_path("/mcp/authorize");
    cookie.set_http_only(true);
    cookie.set_same_site(axum_extra::extract::cookie::SameSite::Lax);
    cookie.set_max_age(AUTHORIZE_FLOW_COOKIE_MAX_AGE);
    let jar = jar.add(cookie);

    let client_name = client.client_name.as_deref().unwrap_or("An application");
    let redirect_host = reqwest::Url::parse(&params.redirect_uri)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
        .unwrap_or_else(|| params.redirect_uri.clone());

    let body = format!(
        "<!doctype html>\n\
         <html><head><meta charset=\"utf-8\"><title>Authorize access</title></head>\n\
         <body style=\"font-family: monospace; max-width: 32rem; margin: 3rem auto;\">\n\
         <p><strong>{client_name}</strong> wants to access your notes as <strong>{user_id}</strong>.</p>\n\
         <p>It will be redirected to: <code>{redirect_host}</code></p>\n\
         <form method=\"post\" action=\"/mcp/authorize/decision\">\n\
         <button type=\"submit\" name=\"decision\" value=\"allow\">Allow</button>\n\
         <button type=\"submit\" name=\"decision\" value=\"deny\">Deny</button>\n\
         </form>\n\
         </body></html>",
        client_name = html_escape(client_name),
        user_id = html_escape(&user_id),
        redirect_host = html_escape(&redirect_host),
    );

    (jar, Html(body)).into_response()
}

/// Minimal percent-encoding for a single query-parameter value (the `next`
/// path+query we build ourselves, not arbitrary user input rendered as
/// markup) - encodes everything outside the RFC 3986 unreserved set.
fn urlencoding_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[derive(Debug, Deserialize)]
struct DecisionForm {
    decision: String,
}

async fn authorize_decision(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    session: tower_sessions::Session,
    Form(form): Form<DecisionForm>,
) -> Response {
    let Some(cookie) = jar.get(AUTHORIZE_FLOW_COOKIE_NAME) else {
        return bad_request_page("missing or expired authorization request - please retry");
    };
    let jar = jar.clone().remove(Cookie::from(AUTHORIZE_FLOW_COOKIE_NAME));

    let flow: AuthorizeFlowState = match serde_json::from_str(cookie.value()) {
        Ok(f) => f,
        Err(_) => return bad_request_page("invalid authorization request - please retry"),
    };

    // Re-confirm the session is still the same user who was shown the
    // consent screen - it's short-lived (5 minutes), but a session change
    // mid-flow (e.g. a logout+different login in another tab) must not let
    // the new session silently inherit a code minted for someone else.
    match session::effective_user_id(&state, &session).await {
        Some(ref uid) if *uid == flow.user_id => {}
        _ => return bad_request_page("session changed during authorization - please retry"),
    }

    if form.decision != "allow" {
        let redirect = append_query_params(
            &flow.redirect_uri,
            &[("error", "access_denied")],
            flow.state.as_deref(),
        );
        return (jar, Redirect::to(&redirect)).into_response();
    }

    let code = match store::mint_auth_code(
        &state.db,
        &flow.client_id,
        &flow.redirect_uri,
        &flow.code_challenge,
        &flow.user_id,
    )
    .await
    {
        Ok(code) => code,
        Err(err) => return server_error("mint_auth_code", err),
    };

    let redirect = append_query_params(
        &flow.redirect_uri,
        &[("code", &code)],
        flow.state.as_deref(),
    );
    (jar, Redirect::to(&redirect)).into_response()
}

/// Append query parameters (plus `state`, if present) onto a redirect_uri
/// that may already carry its own query string. Falls back to the bare
/// `base` string if it somehow isn't a parseable URL - unreachable in
/// practice since every `redirect_uri` reaching here was already validated
/// at registration time, but fails closed rather than panicking.
fn append_query_params(base: &str, pairs: &[(&str, &str)], state: Option<&str>) -> String {
    let Ok(mut url) = reqwest::Url::parse(base) else {
        return base.to_string();
    };
    {
        let mut qp = url.query_pairs_mut();
        for (k, v) in pairs {
            qp.append_pair(k, v);
        }
        if let Some(s) = state {
            qp.append_pair("state", s);
        }
    }
    url.to_string()
}

// ---- POST /mcp/token (RFC 6749) --------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(tag = "grant_type")]
#[serde(rename_all = "snake_case")]
enum TokenRequest {
    AuthorizationCode {
        code: String,
        redirect_uri: String,
        client_id: String,
        code_verifier: String,
    },
    RefreshToken {
        refresh_token: String,
        client_id: String,
    },
}

#[derive(Debug, Serialize)]
struct TokenResponseBody {
    access_token: String,
    token_type: &'static str,
    expires_in: i64,
    refresh_token: String,
}

/// Seconds-until-expiry reported in `expires_in`. Must track
/// `store::ACCESS_TOKEN_TTL`; kept as a separate literal here rather than
/// exported from `store` to avoid coupling that module's internals to the
/// HTTP response shape - if they drift, only this response field is stale,
/// never the actual enforced expiry.
const ACCESS_TOKEN_TTL_SECONDS: i64 = 3600;

async fn token(State(state): State<AppState>, Form(body): Form<TokenRequest>) -> Response {
    match body {
        TokenRequest::AuthorizationCode {
            code,
            redirect_uri,
            client_id,
            code_verifier,
        } => {
            let auth_code = match store::consume_auth_code(&state.db, &code).await {
                Ok(Some(c)) => c,
                Ok(None) => {
                    return oauth_error(
                        StatusCode::BAD_REQUEST,
                        "invalid_grant",
                        "unknown, expired, or already-used code",
                    )
                }
                Err(err) => return server_error("consume_auth_code", err),
            };

            if auth_code.client_id != client_id || auth_code.redirect_uri != redirect_uri {
                return oauth_error(
                    StatusCode::BAD_REQUEST,
                    "invalid_grant",
                    "client_id/redirect_uri do not match the authorization request",
                );
            }
            if !pkce_verifier_matches(&code_verifier, &auth_code.code_challenge) {
                return oauth_error(
                    StatusCode::BAD_REQUEST,
                    "invalid_grant",
                    "code_verifier does not match",
                );
            }

            match store::mint_token_pair(&state.db, &client_id, &auth_code.user_id).await {
                Ok((access_token, refresh_token)) => Json(TokenResponseBody {
                    access_token,
                    token_type: "Bearer",
                    expires_in: ACCESS_TOKEN_TTL_SECONDS,
                    refresh_token,
                })
                .into_response(),
                Err(err) => server_error("mint_token_pair", err),
            }
        }
        TokenRequest::RefreshToken {
            refresh_token,
            client_id,
        } => match store::rotate_refresh_token(&state.db, &refresh_token, &client_id).await {
            Ok(Some((access_token, new_refresh_token))) => Json(TokenResponseBody {
                access_token,
                token_type: "Bearer",
                expires_in: ACCESS_TOKEN_TTL_SECONDS,
                refresh_token: new_refresh_token,
            })
            .into_response(),
            Ok(None) => oauth_error(
                StatusCode::BAD_REQUEST,
                "invalid_grant",
                "unknown, expired, or wrong-client refresh_token",
            ),
            Err(err) => server_error("rotate_refresh_token", err),
        },
    }
}

/// RFC 7636 S256 PKCE verification: the stored `code_challenge` must equal
/// `base64url(SHA-256(code_verifier))`.
fn pkce_verifier_matches(code_verifier: &str, code_challenge: &str) -> bool {
    let computed = URL_SAFE_NO_PAD.encode(Sha256::digest(code_verifier.as_bytes()));
    computed == code_challenge
}

// ---- GET /.well-known/oauth-authorization-server (RFC 8414) ----------------

async fn authorization_server_metadata(State(state): State<AppState>) -> Json<serde_json::Value> {
    let base = state.config.base_url.trim_end_matches('/');
    Json(json!({
        "issuer": base,
        "authorization_endpoint": format!("{base}/mcp/authorize"),
        "token_endpoint": format!("{base}/mcp/token"),
        "registration_endpoint": format!("{base}/mcp/register"),
        "response_types_supported": ["code"],
        "grant_types_supported": ["authorization_code", "refresh_token"],
        "code_challenge_methods_supported": ["S256"],
        "token_endpoint_auth_methods_supported": ["none"],
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_https() {
        assert!(is_acceptable_redirect_uri("https://example.com/callback"));
    }

    #[test]
    fn accepts_loopback_http_any_port() {
        assert!(is_acceptable_redirect_uri("http://127.0.0.1:51234/cb"));
        assert!(is_acceptable_redirect_uri("http://[::1]:9999/cb"));
    }

    #[test]
    fn rejects_non_loopback_http() {
        assert!(!is_acceptable_redirect_uri("http://example.com/cb"));
        assert!(!is_acceptable_redirect_uri("http://192.168.1.5/cb"));
    }

    #[test]
    fn accepts_private_custom_scheme() {
        assert!(is_acceptable_redirect_uri("dev.rustnote.app://auth"));
    }

    #[test]
    fn rejects_malformed_uri() {
        assert!(!is_acceptable_redirect_uri("not a url"));
    }

    #[test]
    fn validate_redirect_uris_rejects_empty_list() {
        assert!(validate_redirect_uris(&[]).is_err());
    }

    #[test]
    fn validate_redirect_uris_rejects_too_many() {
        let uris: Vec<String> = (0..MAX_REDIRECT_URIS + 1)
            .map(|i| format!("https://example.com/{i}"))
            .collect();
        assert!(validate_redirect_uris(&uris).is_err());
    }

    #[test]
    fn validate_redirect_uris_accepts_a_normal_list() {
        assert!(validate_redirect_uris(&["https://example.com/cb".to_string()]).is_ok());
    }

    #[test]
    fn pkce_correct_verifier_matches() {
        // RFC 7636 Appendix B's worked example.
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let challenge = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";
        assert!(pkce_verifier_matches(verifier, challenge));
    }

    #[test]
    fn pkce_wrong_verifier_rejected() {
        assert!(!pkce_verifier_matches(
            "wrong-verifier",
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        ));
    }

    #[test]
    fn append_query_params_preserves_existing_query() {
        let out = append_query_params(
            "https://example.com/cb?already=there",
            &[("code", "abc")],
            Some("xyz"),
        );
        assert!(out.contains("already=there"));
        assert!(out.contains("code=abc"));
        assert!(out.contains("state=xyz"));
    }

    #[test]
    fn urlencoding_encode_escapes_reserved_chars() {
        assert_eq!(
            urlencoding_encode("/mcp/authorize?a=b&c=d"),
            "%2Fmcp%2Fauthorize%3Fa%3Db%26c%3Dd"
        );
    }
}
