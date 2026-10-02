//! LLM calls for the `#AI!` command — a streaming primary path and a
//! one-shot fallback.
//!
//! Both are Bearer `POST`s to `{ai_endpoint}/chat/completions`
//! (OpenAI-compatible), mirroring `todos/query.rs::call_ai`. The ops protocol
//! is **JSONL** (one JSON op per line, no envelope) so that
//! [`call_model_streaming`] can surface each completed op mid-generation for an
//! animated apply; [`call_model`] is the non-streaming fallback used if the
//! endpoint errors on `stream:true`. Neither forces `response_format`
//! (some OpenRouter models reject it); ops are parsed out of the content
//! robustly by [`super::ops`].

use std::time::Duration;

use futures_util::StreamExt;
use serde::Deserialize;
use tokio::sync::mpsc;

/// Upper bound on generated tokens — enough for a handful of edit ops, bounded
/// so a runaway model can't hold the request open or balloon cost.
const MAX_TOKENS: u32 = 2000;

/// Buffered content deltas the SSE reader task can get ahead by.
const STREAM_CHANNEL_CAP: usize = 64;

/// Connect timeout for the outbound AI request (the streaming body itself is
/// bounded by the caller's idle timeout, not a whole-response deadline, so a
/// long-but-progressing generation isn't killed mid-stream).
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// System prompt (validated — see `docs/ai-command.md`). JSONL protocol: one
/// op per line, no wrapping array/object, so ops can be parsed and applied as
/// each line completes mid-stream.
const SYSTEM_PROMPT: &str = r#"You edit a note by emitting precise edit operations, acting as a cursor. You get the CURRENT NOTE and an INSTRUCTION. Emit ALL operations needed to FULLY satisfy the instruction. Respond with ONE JSON operation PER LINE (JSONL) and NOTHING else — no prose, no code fences, no wrapping array or object, no blank lines. Each line is exactly one of: {"op":"replace","find":"…","with":"…"} or {"op":"insert_after","find":"…","text":"…"} or {"op":"append","text":"…"}. Rules: copy each `find` VERBATIM from the note and it MUST occur EXACTLY ONCE; emit every op needed, one per line; any text you insert must include its own leading/trailing newlines (as \n) where a new line is intended; keep edits minimal; each line must be valid JSON on its own; no reasoning, no commentary."#;

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

/// One streamed SSE chunk: `{"choices":[{"delta":{"content":"…"}}]}`.
#[derive(Debug, Deserialize)]
struct ChatStreamChunk {
    #[serde(default)]
    choices: Vec<StreamChoice>,
}
#[derive(Debug, Deserialize)]
struct StreamChoice {
    #[serde(default)]
    delta: StreamDelta,
}
#[derive(Debug, Default, Deserialize)]
struct StreamDelta {
    #[serde(default)]
    content: Option<String>,
}

/// Build the shared request body. `stream` toggles SSE on the wire.
fn request_body(model: &str, body: &str, instruction: &str, stream: bool) -> serde_json::Value {
    let user_message = format!("CURRENT NOTE:\n{body}\n\nINSTRUCTION: {instruction}");
    serde_json::json!({
        "model": model,
        "messages": [
            { "role": "system", "content": SYSTEM_PROMPT },
            { "role": "user", "content": user_message }
        ],
        "temperature": 0,
        "max_tokens": MAX_TOKENS,
        "stream": stream,
    })
}

/// Map a non-success HTTP status to a short, user-safe, secret-free reason.
fn status_reason(status: reqwest::StatusCode) -> String {
    if status.as_u16() == 401 {
        "the AI endpoint rejected the API key".to_string()
    } else {
        format!("the AI endpoint returned {status}")
    }
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
    let request_body = request_body(model, body, instruction, false);

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
        return Err(status_reason(status));
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

/// Start a streaming (`stream:true`) completion. On a successful connect this
/// spawns a background reader that parses the OpenAI-compatible SSE wire format
/// and forwards each `choices[0].delta.content` fragment on the returned
/// channel; the channel closes when the terminal `data: [DONE]` line arrives,
/// the stream ends, or the receiver is dropped. A single forwarded item is
/// `Err(reason)` if the stream itself fails mid-read.
///
/// `Err(reason)` from *this* function means the request could not be started
/// (transport error or a non-success status) — the caller falls back to the
/// one-shot [`call_model`]. Secrets are never included in any reason.
pub async fn call_model_streaming(
    endpoint: &str,
    model: &str,
    key: &str,
    body: &str,
    instruction: &str,
) -> Result<mpsc::Receiver<Result<String, String>>, String> {
    let url = chat_url(endpoint);
    let request_body = request_body(model, body, instruction, true);

    // No whole-response `.timeout()` here: streaming is bounded by the caller's
    // idle timeout so a long progressive generation isn't cut off. Connect is
    // still bounded, and redirects are refused (SSRF), as in `call_model`.
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(CONNECT_TIMEOUT)
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
    if !status.is_success() {
        return Err(status_reason(status));
    }

    let (tx, rx) = mpsc::channel(STREAM_CHANNEL_CAP);
    tokio::spawn(async move {
        let mut stream = resp.bytes_stream();
        // SSE lines don't align to network chunks; buffer and split on '\n',
        // keeping the trailing partial line.
        let mut buf = String::new();
        while let Some(next) = stream.next().await {
            let bytes = match next {
                Ok(b) => b,
                Err(e) => {
                    let _ = tx.send(Err(format!("stream read failed: {e}"))).await;
                    return;
                }
            };
            buf.push_str(&String::from_utf8_lossy(&bytes));
            while let Some(nl) = buf.find('\n') {
                let line: String = buf.drain(..=nl).collect();
                match handle_sse_line(line.trim_end()) {
                    SseLine::Content(content) => {
                        if tx.send(Ok(content)).await.is_err() {
                            return; // receiver gone
                        }
                    }
                    SseLine::Done => return,
                    SseLine::Ignore => {}
                }
            }
        }
    });

    Ok(rx)
}

/// Outcome of interpreting a single SSE line.
enum SseLine {
    Content(String),
    Done,
    Ignore,
}

/// Parse one trimmed SSE line: `data: {json}`, `data: [DONE]`, or anything else
/// (comments, blank lines, event: fields) which is ignored.
fn handle_sse_line(line: &str) -> SseLine {
    let Some(payload) = line.strip_prefix("data:") else {
        return SseLine::Ignore;
    };
    let payload = payload.trim();
    if payload.is_empty() {
        return SseLine::Ignore;
    }
    if payload == "[DONE]" {
        return SseLine::Done;
    }
    match serde_json::from_str::<ChatStreamChunk>(payload) {
        Ok(chunk) => match chunk
            .choices
            .into_iter()
            .next()
            .and_then(|c| c.delta.content)
        {
            Some(content) if !content.is_empty() => SseLine::Content(content),
            _ => SseLine::Ignore, // role-only / finish_reason chunk
        },
        Err(_) => SseLine::Ignore, // keepalive or unexpected shape; skip
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sse_line_parsing_extracts_content_and_terminates() {
        // A content delta.
        match handle_sse_line(r#"data: {"choices":[{"delta":{"content":"{\"op\""}}]}"#) {
            SseLine::Content(c) => assert_eq!(c, "{\"op\""),
            _ => panic!("expected content"),
        }
        // The terminal sentinel.
        assert!(matches!(handle_sse_line("data: [DONE]"), SseLine::Done));
        // Role-only / finish_reason chunks carry no content -> ignored.
        assert!(matches!(
            handle_sse_line(r#"data: {"choices":[{"delta":{}}]}"#),
            SseLine::Ignore
        ));
        assert!(matches!(
            handle_sse_line(r#"data: {"choices":[{"delta":{},"finish_reason":"stop"}]}"#),
            SseLine::Ignore
        ));
        // Blank lines, comments, and non-data fields are ignored.
        assert!(matches!(handle_sse_line(""), SseLine::Ignore));
        assert!(matches!(handle_sse_line(": keepalive"), SseLine::Ignore));
        assert!(matches!(handle_sse_line("event: message"), SseLine::Ignore));
    }

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
