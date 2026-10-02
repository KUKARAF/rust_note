//! One-shot LLM call for the `#AI!` command.
//!
//! Mirrors `todos/query.rs::call_ai`: a single Bearer `POST` to
//! `{ai_endpoint}/chat/completions`, OpenAI-compatible, no streaming. Unlike
//! the todo-query call this deliberately does NOT force
//! `response_format: json_object` (some OpenRouter models reject it); the ops
//! are parsed out of the content robustly by [`super::ops::parse_ops`].

use std::time::Duration;

use serde::Deserialize;

/// Upper bound on generated tokens — enough for a handful of edit ops, bounded
/// so a runaway model can't hold the request open or balloon cost.
const MAX_TOKENS: u32 = 2000;

/// System prompt (validated — see `docs/ai-command.md`).
const SYSTEM_PROMPT: &str = r#"You edit a note by emitting precise edit operations, acting as a cursor. You get the CURRENT NOTE and an INSTRUCTION. Emit ALL operations needed to FULLY satisfy the instruction. Respond with ONLY this JSON, nothing else (no prose, no code fences): {"ops":[ ... ]}. Op types: {"op":"replace","find":"…","with":"…"}, {"op":"insert_after","find":"…","text":"…"}, {"op":"append","text":"…"}. Rules: copy each `find` VERBATIM from the note and it MUST occur EXACTLY ONCE; emit every op needed; any text you insert must include its own leading/trailing newlines where a new line is intended; keep edits minimal; valid JSON only; keep reasoning very short."#;

/// The `{base}/chat/completions` URL for a configured AI endpoint (trailing `/`
/// trimmed), falling back to the default LiteLLM base when the endpoint is
/// empty. Mirrors `todos/query.rs::chat_url`.
fn chat_url(endpoint: &str) -> String {
    let base = endpoint.trim().trim_end_matches('/');
    let base = if base.is_empty() {
        crate::settings::store::DEFAULT_AI_ENDPOINT
    } else {
        base
    };
    format!("{base}/chat/completions")
}

#[derive(Debug, Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
}
#[derive(Debug, Deserialize)]
struct Choice {
    message: ChoiceMessage,
}
#[derive(Debug, Deserialize)]
struct ChoiceMessage {
    content: Option<String>,
}

/// Call the model once and return its raw message content (the ops JSON, to be
/// parsed by [`super::ops::parse_ops`]). `Err(reason)` carries a short,
/// user-safe reason suitable for the `> ⚠️ #AI! couldn't run: …` notice.
pub async fn call_model(
    endpoint: &str,
    model: &str,
    key: &str,
    body: &str,
    instruction: &str,
) -> Result<String, String> {
    let url = chat_url(endpoint);
    let user_message = format!("CURRENT NOTE:\n{body}\n\nINSTRUCTION: {instruction}");

    let request_body = serde_json::json!({
        "model": model,
        "messages": [
            { "role": "system", "content": SYSTEM_PROMPT },
            { "role": "user", "content": user_message }
        ],
        "temperature": 0,
        "max_tokens": MAX_TOKENS
    });

    // Harden the outbound client like the todo-query one: no redirects (SSRF)
    // and a bounded timeout. Only `Authorization: Bearer <key>` +
    // `Content-Type: application/json` are sent.
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| format!("failed to build HTTP client: {e}"))?;

    let resp = client
        .post(&url)
        .bearer_auth(key)
        .json(&request_body)
        .send()
        .await
        .map_err(|e| format!("request to the AI endpoint failed: {e}"))?;

    let status = resp.status();
    let raw = resp
        .text()
        .await
        .map_err(|e| format!("reading the AI endpoint response failed: {e}"))?;

    if !status.is_success() {
        if status.as_u16() == 401 {
            return Err("the AI endpoint rejected the API key".to_string());
        }
        return Err(format!("the AI endpoint returned {status}"));
    }

    let envelope: ChatResponse =
        serde_json::from_str(&raw).map_err(|e| format!("unexpected AI endpoint response: {e}"))?;
    envelope
        .choices
        .into_iter()
        .next()
        .and_then(|c| c.message.content)
        .ok_or_else(|| "the AI endpoint returned no content".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_url_appends_and_trims() {
        assert_eq!(
            chat_url("https://litellm.osmosis.page/v1/"),
            "https://litellm.osmosis.page/v1/chat/completions"
        );
        assert_eq!(
            chat_url(""),
            format!(
                "{}/chat/completions",
                crate::settings::store::DEFAULT_AI_ENDPOINT
            )
        );
    }
}
