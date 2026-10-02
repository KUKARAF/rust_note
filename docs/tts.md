# TTS — read notes (or selections) aloud (contract)

Two entry points, one modal:
1. A **speaker button next to the filename** → reads the WHOLE note.
2. A **floating button that appears next to selected text** in the editor (only when a
   non-empty selection exists) → reads ONLY the selected text.

Both open a **modal** to pick a TTS model (fetched live from litellm) and start playback.

## TTS API (confirmed)

- `POST {ai_endpoint}/audio/speech`, `Authorization: Bearer <key>`, body
  `{"model","input","voice"?}` → returns `audio/wav` bytes. `voice` is **optional**
  (works without it).
- **Voices are NOT enumerable** — litellm has no voices endpoint (all 404). The TTS
  "voices" ARE the models. TTS models are the ones with **`mode == "audio_speech"`** in
  `/v1/models`: `morgan-tts`, `scarlet-tts`, `samantha-tts`. **Fetch them dynamically — never
  hardcode the list.** The modal shows a model picker only; no separate voice dropdown.
- Key lives server-side only → the browser must NOT call litellm directly. A backend proxy
  (`POST /api/tts`) is required.

## Backend — `POST /api/tts`

New `crates/server/src/tts/` module (sibling of `settings`), registered in
`crates/server/src/routes.rs::build` via `.merge(tts::routes::router())` (next to
`settings::routes::router()`), plus `mod tts;` in `main.rs`/lib root.

- Handler: `State(AppState)`, `RequireAuth(user_id)`,
  `WithRejection(Json(TtsRequest), _)` where
  `TtsRequest { text: String, model: String, voice: Option<String> }`.
- Resolve key + endpoint EXACTLY like `settings/routes.rs::get_ai_models`
  (settings key else `config.openrouter_api_key` else `BadRequest`; endpoint
  `settings.ai_endpoint` trimmed). Build `format!("{base}/audio/speech")`.
- **Validate `model` server-side**: it must be a TTS model. Prefer fetching `/v1/models`
  and checking `mode == "audio_speech"` (robust); acceptable fallback: require the id to
  end in `-tts`. Reject otherwise with `BadRequest`.
- POST `{"model": model, "input": text, "voice": voice?}` (omit `voice` if `None`) with
  `.bearer_auth(key).json(..)`.
- **Return the audio:** reqwest `stream` feature is NOT enabled, so buffer with
  `let audio = resp.bytes().await?;` and return
  `([(axum::http::header::CONTENT_TYPE, "audio/wav")], audio)` (tuple-of-headers+Bytes
  implements `IntoResponse`). Errors go through `AppError` (JSON) — the client checks
  `res.ok`/content-type before reading the blob.
- **Timeout:** the shared `recurring::http_client()` has a 20s timeout and the REST layer a
  30s `TimeoutLayer` — a long whole-note synthesis can exceed both. Use a **dedicated TTS
  reqwest client with a longer timeout** (e.g. 90s) AND **cap `text` length** server-side
  (e.g. reject > ~8 KB with a `BadRequest` telling the user to select less / shorten); note
  the REST route still inherits the 30s layer, so keep inputs modest (document the cap).
- No new crates (`reqwest::Response::bytes()`, `bytes::Bytes`, `axum` headers all available).
  No migration.
- (Nice-to-have, optional) extract the duplicated settings-key-then-env-key resolution
  (in `todos/query.rs` + `settings/routes.rs` + now here) into one shared helper.

## Frontend

### 1. Whole-note speaker button
In `web/src/routes/notes/[...path]/+page.svelte`, add a speaker icon button as the first
child of `.editor-header-actions` (mirror the existing `aiEditing` chip / `Track` / `Share`
buttons; gate on `session && editorReady`). On click → open the TTS modal with the whole
note text = `liveContent` (the live editor mirror; fall back to `restContent` if not
mounted). Use `<Button variant="outline" size="sm">🔊</Button>` or a raw icon button like
`.rt-close` in `RecurringTodosDialog.svelte`.

### 2. Selection toolbar (floating button)
In `web/src/lib/editor/CodeMirrorEditor.svelte`, add a CM6 **`showTooltip`-backed floating
button** via a `StateField<readonly Tooltip[]>` fed into the `showTooltip` facet: on each
transaction, if `tr.state.selection.main` is non-empty, produce a
`Tooltip{ pos: sel.head, above: true, create: () => ({ dom: buttonEl }) }`; empty → none.
The button's click **slices the text at click time** from `view.state.sliceDoc(from,to)`
(do NOT cache — remote collab edits shift it) and calls a new component prop
`onSpeakSelection?: (text: string) => void` (added alongside the existing
`onChange`/`onSave`/`onBlur` props). The parent wires `onSpeakSelection` to open the TTS
modal with that text. `@codemirror/view` already exports `showTooltip`/`Tooltip`/`tooltips`;
no new dep. (Rationale: CM owns positioning + reposition-on-scroll; a manual DOM overlay or
`window.getSelection()` is unreliable under CM's `drawSelection`.)

### 3. TtsDialog modal (new `web/src/lib/notes/TtsDialog.svelte`)
Copy the `RecurringTodosDialog.svelte` pattern exactly: parent `$state` boolean
`ttsModalOpen` + `{#if ttsModalOpen}<TtsDialog text={...} {onclose} />`; fixed `.rt-backdrop`
→ `.rt-wrap` → `<Card>`; Escape + backdrop-click close; `onclose` prop (match the two most
recent dialogs' casing). Contents:
- **Model picker** (dynamically populated): call `fetchAiModels()` (from
  `$lib/stores/settings`), filter to TTS models (`mode==audio_speech` isn't in that payload,
  so filter ids ending in `-tts`, OR add a tiny `/api/ai/models` passthrough is unnecessary —
  id-suffix filter is fine). Handle the no-key degraded state (`error` non-null → show
  "set an API key in Settings first" and disable Play), same branch the Settings page uses.
  Default-select the first tts model (or a stored default if we add one).
- **Play** (primary `Button`): POST `/api/tts` with `{text, model}` via a **raw `fetch`**
  against `API_BASE_URL` attaching auth the SAME way the API client does (bearer in app
  mode — reuse/extend `web/src/lib/api/client.ts`; do NOT use `apiPost` which assumes JSON).
  Check `res.ok` before `await res.blob()`; `URL.createObjectURL(blob)` → bind to an
  `<audio controls autoplay?>` element (user-gesture triggered — don't autoplay on open;
  play on the Play click). Show loading + error states. `URL.revokeObjectURL` on close/replace.
- Tauri (`IS_APP`): the raw fetch must carry the bearer token; audio plays in the WebView but
  must be user-gesture-initiated (the Play click satisfies this).

### Optional persisted default
If we want to remember the chosen TTS model, add a `tts_model` per-user setting via the same
5-touchpoint recipe as `ai_command_model` (store struct+default, PUT set+validate, GET
response, settings.ts state+setter, a Settings control). **Optional for v1** — default to the
first fetched tts model is fine.

## Constraints (CI)
`cargo fmt`; `cargo clippy -p server -p rust-note-core --lib --bins -- -D warnings` clean;
`cargo test` green (add a test for model validation + the key/endpoint URL build);
`npm run check` + `npm run build` green. No new crates, no migration. rustc 1.94. Build cargo
in `nix-toolbox-44`.

## Build decomposition (coordinator note)
Spawn AFTER the #AI! backend (cargo) and pipeline-stats (npm) agents land + integrate, so
builds don't contend in the shared tree. Then:
- **Agent 1 (backend, cargo):** the `tts` module + `POST /api/tts` + model validation +
  dedicated client/timeout + input cap + route registration + a unit test.
- **Agent 2 (frontend, npm):** `TtsDialog.svelte` + the `fetchAiModels` filter + raw-fetch
  playback; the `onSpeakSelection` prop + `showTooltip` selection button in
  `CodeMirrorEditor.svelte`; the filename speaker button + modal wiring in the notes page.
Backend (cargo) + frontend (npm) use different toolchains → safe to run concurrently in the
same tree.

## Chunked, progressively-buffered playback (current design)

The original v1 shipped a single "synthesize the whole note in one request" call. That meant
no audio played until the ENTIRE note finished rendering server-side, a slow/failed upstream
call killed playback of the whole note, and the REST layer's (now-removed) 30s `TimeoutLayer`
could 408 a long note outright. `/api/tts` is now called once PER CHUNK of a small, pre-split
text, and the frontend buffers a few chunks ahead of what's currently playing.

### Frontend chunking — `web/src/lib/notes/ttsChunk.ts`

Pure, dependency-free, unit-tested module (`ttsChunk.test.ts`, run via
`node --experimental-strip-types src/lib/notes/ttsChunk.test.ts` / `npm run test:unit` — there
is no vitest/jest in this project, so this is a plain script with hand-rolled assertions rather
than a new devDependency for one pure-function module).

`chunkText(text, opts?) -> { chunks: string[], truncated: boolean, totalWords: number }`:
1. Split on blank-line paragraph boundaries.
2. Normalize whitespace and strip a few trivial markdown markers that sound jarring read
   verbatim (`#` headers, `- `/`* ` bullets, `> ` quotes, `**bold**`/`_italic_`/`` `code` ``
   wrappers) — not a full markdown parser, intentionally shallow.
3. Within an over-long paragraph, split on sentence boundaries (`.`/`!`/`?` + whitespace),
   greedily packing sentences up to `MAX_WORDS_PER_CHUNK` (default **50**) words per chunk.
   A single sentence that alone exceeds the cap is hard-split on word boundaries.
4. Enforce `MAX_TOTAL_WORDS` (default **4000**) as a safety net across the whole input: once
   adding the next chunk would exceed the budget, chunking stops and `truncated: true` is
   returned (not an error) so the caller can synthesize the prefix and tell the user the rest
   was skipped.

Defaults rationale: ~50 words/chunk synthesizes in roughly 1-3s on a typical TTS model, which
comfortably fits inside the frontend's 2-3 chunk prefetch window at normal speech rate
(~130-160 wpm) while staying large enough to avoid excessive per-chunk HTTP overhead or choppy
audio. 4000 words is well over half an hour of speech — generous for a note, but still a hard
backstop against pathological input (e.g. an entire vault pasted in as "the note").

### Frontend playback — `web/src/lib/notes/TtsDialog.svelte`

- `chunks[]` from `chunkText`, each with a `ChunkSlot { status: idle|loading|ready|error|done,
  url, error }`.
- `ensurePrefetch()` kicks off a `rawFetch('/api/tts', { body: { text: chunk, model } })` for
  every idle slot in `[currentIndex, currentIndex + PREFETCH_AHEAD]` (`PREFETCH_AHEAD = 2`,
  i.e. up to 3 chunks buffered: current + 2 ahead). Each fetch checks `res.ok` and a
  `content-type` starting with `audio/` before `res.blob()`, then `URL.createObjectURL`s it.
- Playback starts as soon as chunk 0's blob is ready (doesn't wait for the rest) through ONE
  `<audio>` element. On `ended`: revoke the finished chunk's object URL, advance
  `currentIndex`, and either start the next chunk (if its slot is already `ready`) or wait —
  a `$effect` watching `(playState, slots[currentIndex])` starts it the moment it becomes
  ready. Small gaps at sentence boundaries are expected; no Web Audio gapless stitching.
- **Controls:** Play/Pause/Resume toggle + Stop, plus a "chunk N / M" progress line. Stop
  (and modal close/unmount, and entering the error state) call `abortAndRevokeAll()`: aborts
  every in-flight fetch via its own `AbortController` and revokes every outstanding object
  URL. A per-chunk fetch error surfaces which chunk failed and stops cleanly rather than
  silently skipping it.
- Tauri/WebView: the first `Play` click is the user gesture; subsequent chunk starts are
  chained from the `ended` event of an already-playing element, which browsers (and the
  Tauri WebView) allow. Still uses `rawFetch` so the bearer device token is attached in app
  mode.

### Backend tweak — `crates/server/src/tts/routes.rs`

- `MAX_TTS_TEXT_BYTES` lowered from 8 KiB to **4 KiB**: chunks are now small (tens of words),
  so a single request never carries a whole note; the cap mainly guards against a caller that
  bypasses the chunker.
- `TTS_TIMEOUT` raised from 90s to **120s**: `/api/tts` no longer sits behind the removed 30s
  `TimeoutLayer`, and a single chunk can still be slow on a cold/loaded TTS backend.
- Text-length validation was pulled into a standalone `validate_text_len` function so it's
  unit-testable without standing up `AppState`; see `tts::routes::tests` for cap-boundary and
  typical-chunk-size coverage.
