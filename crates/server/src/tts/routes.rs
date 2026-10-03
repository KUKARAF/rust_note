//! `POST /api/tts` — proxy text to the user's OpenAI-compatible
//! `audio/speech` endpoint and return the synthesized `audio/wav` bytes.
//!
//! The AI API key is resolved server-side (settings, else env) exactly like
//! `settings::routes::get_ai_models` and only ever rides in the
//! `Authorization` header — never a URL, response body, or log line.

use std::time::Duration;

use anyhow::anyhow;
use axum::extract::State;
use axum::response::IntoResponse;
use axum::routing::post;
use axum::{Json, Router};
use axum_extra::extract::WithRejection;
use serde::Deserialize;

use crate::auth::session::RequireAuth;
use crate::error::{AppError, AppResult};
use crate::settings::store;
use crate::state::AppState;

/// Upper bound on the text accepted for a single synthesis request. The
/// frontend now splits notes into small chunks (a few sentences each, see
/// `web/src/lib/notes/ttsChunk.ts`) before calling this endpoint, so a single
/// request is never a whole note anymore — 4 KiB is generous headroom for one
/// chunk while still catching a caller that bypasses the chunker.
const MAX_TTS_TEXT_BYTES: usize = 4 * 1024;

/// Dedicated TTS client timeout. Longer than the shared `recurring::http_client`
/// (20s): even a single small chunk can be slow on a cold/loaded TTS backend,
/// and this route no longer sits behind the REST layer's `TimeoutLayer`.
const TTS_TIMEOUT: Duration = Duration::from_secs(120);

pub fn router() -> Router<AppState> {
    Router::new().route("/api/tts", post(synthesize))
}

#[derive(Debug, Deserialize)]
struct TtsRequest {
    text: String,
    model: String,
    #[serde(default)]
    voice: Option<String>,
}

/// OpenAI-compatible `GET {base}/models` envelope. Only `id` and the LiteLLM
/// `mode` discriminator are needed to confirm a model is a TTS model.
#[derive(Debug, Deserialize)]
struct ModelsEnvelope {
    data: Vec<ModelEntry>,
}

#[derive(Debug, Deserialize)]
struct ModelEntry {
    id: String,
    #[serde(default)]
    mode: Option<String>,
}

async fn synthesize(
    State(state): State<AppState>,
    RequireAuth(user_id): RequireAuth,
    WithRejection(Json(body), _): WithRejection<Json<TtsRequest>, AppError>,
) -> AppResult<impl IntoResponse> {
    validate_text_len(&body.text)?;

    let note_id = store::settings_note_id(&user_id);
    let settings = {
        let _guard = state.note_locks.lock(&note_id).await;
        store::load_or_bootstrap(&state, &user_id).await?
    };

    // Resolve the key like `get_ai_models`, but only send the shared deployment
    // key to the default endpoint — a custom endpoint requires the user's own
    // key (else the deployment secret leaks). The key only ever rides in the
    // Authorization header below.
    let key = store::resolve_ai_key(
        &settings.openrouter_api_key,
        &settings.ai_endpoint,
        state.config.openrouter_api_key.as_deref(),
    )
    .map_err(AppError::BadRequest)?;

    let endpoint = settings.ai_endpoint.as_str();

    // SSRF guard: refuse loopback/private/link-local/metadata targets before any
    // outbound request. `validate_tts_model` (below) and the speech POST share a
    // host, so one guard on the endpoint covers both.
    store::guard_outbound_url(&speech_url(endpoint))
        .await
        .map_err(AppError::BadRequest)?;

    // Reject non-TTS models server-side so we never send a (potentially huge)
    // text body to a chat model.
    validate_tts_model(endpoint, &body.model, &key).await?;

    let url = speech_url(endpoint);
    // Omit `voice` entirely when absent — the endpoint works without it.
    let request_body = match &body.voice {
        Some(voice) => serde_json::json!({
            "model": body.model,
            "input": body.text,
            "voice": voice,
        }),
        None => serde_json::json!({
            "model": body.model,
            "input": body.text,
        }),
    };

    let client = tts_client()?;
    let resp = client
        .post(&url)
        .bearer_auth(&key)
        .json(&request_body)
        .send()
        .await
        .map_err(|e| AppError::Internal(anyhow!("request to the TTS endpoint failed: {e}")))?;

    let status = resp.status();
    if !status.is_success() {
        if status.as_u16() == 401 {
            return Err(AppError::BadRequest(
                "The AI endpoint rejected the API key.".to_string(),
            ));
        }
        // Don't echo the upstream body — keep it short and key-free.
        return Err(AppError::Internal(anyhow!(
            "the TTS endpoint returned {status}"
        )));
    }

    // The reqwest `stream` feature is NOT enabled, so buffer the whole body.
    let audio = resp
        .bytes()
        .await
        .map_err(|e| AppError::Internal(anyhow!("reading the TTS audio failed: {e}")))?;

    Ok(([(axum::http::header::CONTENT_TYPE, "audio/wav")], audio))
}

/// Reject empty input and input over [`MAX_TTS_TEXT_BYTES`]. Pulled out of the
/// handler so it's unit-testable without standing up `AppState`.
fn validate_text_len(text: &str) -> AppResult<()> {
    if text.trim().is_empty() {
        return Err(AppError::BadRequest("empty text".to_string()));
    }
    if text.len() > MAX_TTS_TEXT_BYTES {
        return Err(AppError::BadRequest(format!(
            "text is too long ({} bytes); the limit is {MAX_TTS_TEXT_BYTES} bytes per request — \
             the client should send smaller chunks",
            text.len()
        )));
    }
    Ok(())
}

/// Confirm `model` is a text-to-speech model on the user's endpoint. Prefers the
/// authoritative `{endpoint}/models` list (`mode == "audio_speech"`); falls back
/// to the `-tts` id convention only when the list can't be fetched/parsed or a
/// matched entry carries no `mode`. Rejects with `BadRequest` otherwise. The key
/// rides only in the Authorization header.
async fn validate_tts_model(endpoint: &str, model: &str, key: &str) -> AppResult<()> {
    let url = models_url(endpoint);
    let is_tts = match crate::recurring::http_client() {
        Ok(client) => match client.get(&url).bearer_auth(key).send().await {
            Ok(resp) if resp.status().is_success() => match resp.json::<ModelsEnvelope>().await {
                Ok(env) => model_is_tts(&env.data, model),
                // Couldn't parse the list — fall back to the id-suffix heuristic.
                Err(_) => model.ends_with("-tts"),
            },
            // Upstream unreachable or non-2xx — fall back to the id-suffix heuristic.
            _ => model.ends_with("-tts"),
        },
        Err(_) => model.ends_with("-tts"),
    };

    if is_tts {
        Ok(())
    } else {
        Err(AppError::BadRequest(format!(
            "'{model}' is not a text-to-speech model"
        )))
    }
}

/// Whether the requested `model` is a TTS model given the endpoint's model list.
/// A matched entry with an explicit `mode` is trusted (`audio_speech` only);
/// a matched entry without a `mode` falls back to the `-tts` id convention; an
/// unknown model is rejected.
fn model_is_tts(entries: &[ModelEntry], model: &str) -> bool {
    match entries.iter().find(|e| e.id == model) {
        None => false,
        Some(entry) => match entry.mode.as_deref() {
            Some(mode) => mode == "audio_speech",
            None => model.ends_with("-tts"),
        },
    }
}

/// The endpoint base: trimmed, trailing `/` removed, falling back to the default
/// LiteLLM base if empty (mirrors `todos::query::chat_url`).
fn base(endpoint: &str) -> &str {
    let base = endpoint.trim().trim_end_matches('/');
    if base.is_empty() {
        store::DEFAULT_AI_ENDPOINT
    } else {
        base
    }
}

/// `{base}/audio/speech` for a configured AI endpoint.
fn speech_url(endpoint: &str) -> String {
    format!("{}/audio/speech", base(endpoint))
}

/// `{base}/models` for a configured AI endpoint (used to validate the model).
fn models_url(endpoint: &str) -> String {
    format!("{}/models", base(endpoint))
}

/// Dedicated outbound client for the (potentially slow) synthesis POST: no
/// redirects (SSRF guard) and a long timeout. Mirrors `recurring::http_client`
/// but with [`TTS_TIMEOUT`] instead of 20s.
fn tts_client() -> AppResult<reqwest::Client> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(TTS_TIMEOUT)
        .build()
        .map_err(|e| AppError::Internal(anyhow!("failed to build TTS HTTP client: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speech_url_appends_path_and_trims_slash() {
        assert_eq!(
            speech_url("https://litellm.osmosis.page/v1"),
            "https://litellm.osmosis.page/v1/audio/speech"
        );
        // Trailing slash (and surrounding whitespace) is trimmed before append.
        assert_eq!(
            speech_url("  https://litellm.osmosis.page/v1/  "),
            "https://litellm.osmosis.page/v1/audio/speech"
        );
        // Empty endpoint falls back to the default LiteLLM base.
        assert_eq!(
            speech_url(""),
            format!("{}/audio/speech", store::DEFAULT_AI_ENDPOINT)
        );
    }

    #[test]
    fn validate_text_len_rejects_empty_and_whitespace_only() {
        assert!(matches!(
            validate_text_len(""),
            Err(AppError::BadRequest(_))
        ));
        assert!(matches!(
            validate_text_len("   \n\t  "),
            Err(AppError::BadRequest(_))
        ));
    }

    #[test]
    fn validate_text_len_accepts_up_to_the_cap_and_rejects_over() {
        let at_cap = "a".repeat(MAX_TTS_TEXT_BYTES);
        assert!(validate_text_len(&at_cap).is_ok());

        let over_cap = "a".repeat(MAX_TTS_TEXT_BYTES + 1);
        assert!(matches!(
            validate_text_len(&over_cap),
            Err(AppError::BadRequest(_))
        ));
    }

    #[test]
    fn validate_text_len_accepts_a_typical_chunk() {
        // A single ~50-word chunk (see web/src/lib/notes/ttsChunk.ts) is nowhere
        // near MAX_TTS_TEXT_BYTES — confirms the lowered cap still fits real chunks.
        let chunk = "word ".repeat(50);
        assert!(validate_text_len(&chunk).is_ok());
    }

    #[test]
    fn models_url_builds_from_endpoint() {
        assert_eq!(
            models_url("https://litellm.osmosis.page/v1/"),
            "https://litellm.osmosis.page/v1/models"
        );
    }

    #[test]
    fn model_is_tts_prefers_mode_then_suffix() {
        let entries = vec![
            ModelEntry {
                id: "morgan-tts".into(),
                mode: Some("audio_speech".into()),
            },
            ModelEntry {
                id: "gpt-oss-20b".into(),
                mode: Some("chat".into()),
            },
            ModelEntry {
                id: "legacy-tts".into(),
                mode: None,
            },
            ModelEntry {
                id: "weird-name".into(),
                mode: None,
            },
        ];

        // Explicit `audio_speech` mode → TTS.
        assert!(model_is_tts(&entries, "morgan-tts"));
        // Explicit non-audio mode → not TTS (trust the mode).
        assert!(!model_is_tts(&entries, "gpt-oss-20b"));
        // Mode absent but id ends with `-tts` → TTS (documented fallback).
        assert!(model_is_tts(&entries, "legacy-tts"));
        // Mode absent and no `-tts` suffix → not TTS.
        assert!(!model_is_tts(&entries, "weird-name"));
        // Model not in the list at all → not TTS.
        assert!(!model_is_tts(&entries, "does-not-exist"));
    }
}
