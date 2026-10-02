# Route the app's AI through the user's LiteLLM proxy (contract)

Today the Todos natural-language query calls **OpenRouter** directly
(`crates/server/src/todos/query.rs`, hardcoded `https://openrouter.ai/api/v1/chat/completions`).
Point it at the user's self-hosted **LiteLLM** proxy instead. LiteLLM is OpenAI-compatible,
so this is a base-URL + key + model-list change. No new crates, no DB migration.

**Validated against the live proxy** (`litellm.osmosis.page`):
- Chat endpoint: `POST {base}/chat/completions` where `base = https://litellm.osmosis.page/v1`. OpenAI-compatible request/response. Auth `Authorization: Bearer <litellm-key>`. No OpenRouter-specific headers needed.
- Model list: `GET {base}/models` → OpenAI shape `{data:[{id,...}]}` — **502 models** on this proxy (13 local incl. `gpt-oss-20b`, plus ~489 OpenRouter-proxied). Needs the same Bearer key.
- `gpt-oss-20b` returns clean completions → use it as the **default model**.

## Keep internal names (no settings migration)

Do NOT rename the existing `openrouter_api_key` / `openrouter_model` frontmatter keys or
struct fields — they just mean "the AI key / model" and the prod settings note already uses
them (key currently empty). Only **add** `ai_endpoint` and **relabel the UI**.

## Backend

1. `crates/server/src/settings/store.rs`
   - Add `ai_endpoint: String` to `UserSettings` (frontmatter key `ai_endpoint`), default
     `https://litellm.osmosis.page/v1`. Tolerant parse like the other fields.
   - Change `DEFAULT_OPENROUTER_MODEL` to `gpt-oss-20b` (the bootstrap/default model; existing
     notes keep whatever they stored — the user re-picks via the new picker).
2. `crates/server/src/settings/routes.rs`
   - `SettingsResponse` gains `ai_endpoint` (non-secret). `PutSettingsRequest` gains
     `ai_endpoint: Option<String>` with **http(s) URL validation** (`reqwest::Url`; non-http(s)
     → `AppError::BadRequest`). Persist via `fm.set` like the others.
3. `crates/server/src/todos/query.rs`
   - Build the chat URL from settings: `{ai_endpoint, trailing '/' trimmed}/chat/completions`
     (fallback to the default base if somehow empty). Keep the key resolution as-is
     (`settings.openrouter_api_key` else `config.openrouter_api_key`).
   - Send only `Authorization: Bearer <key>` + `Content-Type: application/json`. **Remove any
     OpenRouter-specific headers** (`HTTP-Referer`, `X-Title`) if present. Request/response
     parsing is unchanged (OpenAI chat completions). Reword user-facing error strings from
     "OpenRouter" to "the AI endpoint" / "LiteLLM".
4. **New route `GET /api/ai/models`** (authed, current user): load settings; if
   `openrouter_api_key` (resolved, incl. env fallback) is empty → return
   `{ "models": [], "error": "set an API key first" }` (HTTP 200, graceful). Else
   `GET {ai_endpoint}/models` with the Bearer key (reuse `recurring::http_client` or reqwest),
   parse `data[].id`, return `{ "models": [sorted ids], "error": null }`. On upstream
   non-2xx / network / parse error → `{ "models": [], "error": "<short message>" }` (HTTP 200,
   never 500, never leak the key). Register alongside the settings routes.
5. Tests: `ai_endpoint` parse/round-trip + default; endpoint URL validation (reject
   `ftp://`/garbage); the chat URL assembly (`.../v1` + `/chat/completions`).

Secret safety: the AI key must never appear in any response body or log line.

## Frontend (`web/src/routes/settings/+page.svelte` + `web/src/lib/stores/settings.ts`)

Mirror the existing AI block; relabel it **`AI · LITELLM`** and update the description
("Powers the natural-language query on the Todos board, via your LiteLLM proxy. The key is
stored on the server and never shown again.").

- Add `ai_endpoint` to the settings store + a setter (PUT single field, like the others).
  Add an `ENDPOINT` text field (default `https://litellm.osmosis.page/v1`).
- **Replace the 4 model pills** with a **searchable type-ahead** picker (502 options): an
  `Input` wired to a native `<datalist>` (or the app's existing combobox pattern) whose
  options come from `GET /api/ai/models`, fetched on mount **when a key is set** and re-fetched
  after the key or endpoint is saved. Selecting/typing a value persists to `openrouter_model`
  via the existing model setter. If no key (or the endpoint returns `error`), show the muted
  hint from the `error` string ("set an API key first") and fall back to a plain text input
  pre-filled with the current `openrouter_model`. Show the active model under the field.
- Keep the write-only API-key field + Save + `has_openrouter_key` status (now the LiteLLM key).

## Constraints
Same CI gate as the rest: `cargo fmt` clean, `cargo clippy -p server -p rust-note-core --lib --bins -- -D warnings` zero warnings, `cargo test -p rust-note-core -p server` green; `npm run check` + `npm run build` green. No new crates, no migration. Build cargo inside `nix-toolbox-44` (`toolbox run --container nix-toolbox-44 ...`); `target/` is populated. No git.
