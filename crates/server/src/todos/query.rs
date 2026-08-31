//! `POST /api/todos/query` — turn a natural-language request into a structured
//! [`QuerySpec`] via OpenRouter. The spec is returned to the client and applied
//! there (see web `$lib/notes/todos.ts`); this endpoint never returns the todo
//! list itself, so the LLM only ever sees the query text, not the notes.

use std::time::Duration;

use anyhow::anyhow;
use axum::extract::State;
use axum::routing::post;
use axum::{Json, Router};
use axum_extra::extract::WithRejection;
use serde::{Deserialize, Serialize};

use crate::auth::session::RequireAuth;
use crate::error::{AppError, AppResult};
use crate::settings::store;
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new().route("/api/todos/query", post(query_todos))
}

#[derive(Debug, Deserialize)]
struct QueryRequest {
    nl: String,
}

/// Structured query the client applies to its todo list. Field names match the
/// TypeScript `QuerySpec` (camelCase) so the response is consumed as-is.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct QuerySpec {
    #[serde(skip_serializing_if = "Option::is_none", default)]
    text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    burners: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    tags: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    locations: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pomodoros_min: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pomodoros_max: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    sort: Option<Vec<SortKey>>,
}

#[derive(Debug, Serialize, Deserialize)]
struct SortKey {
    field: String,
    dir: String,
}

const BURNERS: &[&str] = &["frontburner", "backburner", "fridge", "oven"];
const STATUSES: &[&str] = &["open", "done", "all"];
const SORT_FIELDS: &[&str] = &["burner", "pomodoros", "date", "start", "due"];

impl QuerySpec {
    /// Drop anything the LLM produced that isn't a valid enum value, so a
    /// hallucinated burner/status/sort field can't silently break the
    /// client-side filter (it would match nothing).
    fn sanitized(mut self) -> Self {
        self.burners = self.burners.map(|bs| {
            bs.into_iter()
                .map(|b| b.to_ascii_lowercase())
                .filter(|b| BURNERS.contains(&b.as_str()))
                .collect()
        });
        // Locations are an open vocabulary (not an enum), so just normalize:
        // lowercase to match the parser, trim the `@` and stray punctuation,
        // and drop anything that empties out.
        self.locations = self.locations.map(|ls| {
            ls.into_iter()
                .map(|l| {
                    l.trim_start_matches('@')
                        .trim_matches(|c: char| !c.is_alphanumeric() && c != '_' && c != '-')
                        .to_ascii_lowercase()
                })
                .filter(|l| !l.is_empty())
                .collect()
        });
        self.status = self
            .status
            .map(|s| s.to_ascii_lowercase())
            .filter(|s| STATUSES.contains(&s.as_str()));
        self.sort = self.sort.map(|keys| {
            keys.into_iter()
                .filter_map(|k| {
                    let field = k.field.to_ascii_lowercase();
                    if !SORT_FIELDS.contains(&field.as_str()) {
                        return None;
                    }
                    let dir = match k.dir.to_ascii_lowercase().as_str() {
                        "desc" => "desc",
                        _ => "asc",
                    };
                    Some(SortKey {
                        field,
                        dir: dir.to_string(),
                    })
                })
                .collect()
        });
        self
    }
}

const SYSTEM_PROMPT: &str = r#"You translate a user's natural-language request about their todo list into a JSON query. Respond with ONLY a JSON object, no prose, no markdown fences.

The todo items come from daily notes and have these fields:
- text: the task description
- burner: kitchen-priority, one of "frontburner" (do now), "backburner" (simmering, lower priority), "fridge" (parked, needs attention before it spoils), "oven" (eventually). May be absent.
- pomodoros: integer effort estimate (from a `p:N` token). May be absent.
- start: time-of-day "HH:MM". May be absent.
- due: freeform due text. May be absent.
- tags: list of hashtags (without the #).
- locations: GTD-style contexts from `@` tokens (e.g. @work, @home, @store), without the @. May be absent.
- date: the daily-note date "YYYY-MM-DD".
- done: whether completed.

Output JSON schema (all fields optional — include only what the request implies):
{
  "text": string,                 // substring to match in the task text
  "burners": string[],            // subset of the four burners above
  "tags": string[],               // tags that must all be present (no leading #)
  "locations": string[],          // contexts; a task matches ANY of these (no leading @). Tasks with no location always stay visible.
  "status": "open" | "done" | "all",
  "pomodorosMin": number,
  "pomodorosMax": number,
  "sort": [ { "field": "burner"|"pomodoros"|"date"|"start"|"due", "dir": "asc"|"desc" } ]
}

Examples:
- "fridge stuff, most pomodoros first" -> {"burners":["fridge"],"sort":[{"field":"pomodoros","dir":"desc"}]}
- "open frontburner tasks" -> {"burners":["frontburner"],"status":"open"}
- "quick wins" -> {"pomodorosMax":1,"status":"open"}
- "stuff to do at the store" -> {"locations":["store"]}
- "errands at home or work" -> {"locations":["home","work"]}
- "what's due, soonest first" -> {"sort":[{"field":"due","dir":"asc"}]}"#;

async fn query_todos(
    State(state): State<AppState>,
    RequireAuth(user_id): RequireAuth,
    WithRejection(Json(body), _): WithRejection<Json<QueryRequest>, AppError>,
) -> AppResult<Json<QuerySpec>> {
    if body.nl.trim().is_empty() {
        return Err(AppError::BadRequest("empty query".to_string()));
    }

    let note_id = store::settings_note_id(&user_id);
    let settings = {
        let _guard = state.note_locks.lock(&note_id).await;
        store::load_or_bootstrap(&state, &user_id).await?
    };

    let key = if !settings.openrouter_api_key.is_empty() {
        settings.openrouter_api_key.clone()
    } else if let Some(k) = &state.config.openrouter_api_key {
        k.clone()
    } else {
        return Err(AppError::BadRequest(
            "No OpenRouter API key configured — add one in Settings.".to_string(),
        ));
    };

    let spec = call_openrouter(&settings.openrouter_model, &key, body.nl.trim()).await?;
    Ok(Json(spec))
}

#[derive(Debug, Deserialize)]
struct OpenRouterResponse {
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

async fn call_openrouter(model: &str, key: &str, nl: &str) -> AppResult<QuerySpec> {
    let request_body = serde_json::json!({
        "model": model,
        "messages": [
            { "role": "system", "content": SYSTEM_PROMPT },
            { "role": "user", "content": nl }
        ],
        "response_format": { "type": "json_object" },
        "temperature": 0
    });

    // Harden the outbound client like the OIDC one: no redirects (SSRF), and a
    // bounded timeout so a slow provider can't hold a request open.
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|e| AppError::Internal(anyhow!("failed to build HTTP client: {e}")))?;

    let resp = client
        .post("https://openrouter.ai/api/v1/chat/completions")
        .bearer_auth(key)
        .json(&request_body)
        .send()
        .await
        .map_err(|e| AppError::Internal(anyhow!("OpenRouter request failed: {e}")))?;

    let status = resp.status();
    let raw = resp
        .text()
        .await
        .map_err(|e| AppError::Internal(anyhow!("reading OpenRouter response failed: {e}")))?;

    if !status.is_success() {
        if status.as_u16() == 401 {
            return Err(AppError::BadRequest(
                "OpenRouter rejected the API key.".to_string(),
            ));
        }
        return Err(AppError::Internal(anyhow!(
            "OpenRouter returned {status}: {raw}"
        )));
    }

    let envelope: OpenRouterResponse = serde_json::from_str(&raw)
        .map_err(|e| AppError::Internal(anyhow!("unexpected OpenRouter response: {e}")))?;
    let content = envelope
        .choices
        .into_iter()
        .next()
        .and_then(|c| c.message.content)
        .ok_or_else(|| AppError::Internal(anyhow!("OpenRouter returned no content")))?;

    let json = extract_json(&content)
        .ok_or_else(|| AppError::Internal(anyhow!("no JSON object in model output")))?;
    let spec: QuerySpec = serde_json::from_str(json)
        .map_err(|e| AppError::Internal(anyhow!("could not parse the model's query JSON: {e}")))?;
    Ok(spec.sanitized())
}

/// Pull the first `{ … }` JSON object out of `content`, tolerating a model that
/// wrapped it in prose or ```json fences.
fn extract_json(content: &str) -> Option<&str> {
    let start = content.find('{')?;
    let end = content.rfind('}')?;
    if end < start {
        return None;
    }
    content.get(start..=end)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_json_handles_fenced_output() {
        let fenced = "```json\n{\"status\":\"open\"}\n```";
        assert_eq!(extract_json(fenced), Some("{\"status\":\"open\"}"));
        assert_eq!(extract_json("no json here"), None);
    }

    #[test]
    fn sanitize_drops_invalid_enum_values() {
        let spec = QuerySpec {
            burners: Some(vec!["fridge".into(), "microwave".into()]),
            status: Some("someday".into()),
            sort: Some(vec![
                SortKey {
                    field: "pomodoros".into(),
                    dir: "DESC".into(),
                },
                SortKey {
                    field: "bogus".into(),
                    dir: "asc".into(),
                },
            ]),
            ..Default::default()
        }
        .sanitized();

        assert_eq!(spec.burners, Some(vec!["fridge".to_string()]));
        assert_eq!(spec.status, None); // "someday" dropped

        // Locations: `@` stripped, lowercased, empties dropped (open vocabulary).
        let loc = QuerySpec {
            locations: Some(vec!["@Work".into(), "store".into(), "@".into()]),
            ..Default::default()
        }
        .sanitized();
        assert_eq!(
            loc.locations,
            Some(vec!["work".to_string(), "store".to_string()])
        );
        let sort = spec.sort.unwrap();
        assert_eq!(sort.len(), 1);
        assert_eq!(sort[0].field, "pomodoros");
        assert_eq!(sort[0].dir, "desc");
    }

    #[test]
    fn spec_serializes_to_camel_case_and_omits_none() {
        let spec = QuerySpec {
            pomodoros_min: Some(2),
            status: Some("open".into()),
            ..Default::default()
        };
        let json = serde_json::to_string(&spec).unwrap();
        assert!(json.contains("\"pomodorosMin\":2"));
        assert!(json.contains("\"status\":\"open\""));
        assert!(!json.contains("burners"));
    }
}
