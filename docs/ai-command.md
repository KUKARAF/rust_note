# `#AI!` — in-note AI editing (contract)

When a note contains a line ending in `#AI!`, that line is an **instruction**. The
server lifts it out, sends it + the note body to an LLM, and applies the LLM's edits back
into the note's **live Yjs doc as a cursor-driven collaborator** — so every connected
editor watches the note get edited in real time.

**Approach (validated empirically — see the benchmark):** the LLM returns a small set of
**anchored find/replace ops** (NOT a whole-note rewrite). The server validates each anchor
and applies it as a minimal CRDT splice, moving an "AI" cursor (awareness) to each edit.
This is surgical, reuses the battle-tested `collab/write.rs` splice+broadcast path, and
needs **no new deps and no streaming** (v1 is one-shot generation + *paced* application).

## Model

New per-user setting **`ai_command_model`**, default **`openrouter/google/gemini-3.1-flash-lite`**
(benchmarked 6/6 clean @ ~1s). Reuses the existing `ai_endpoint` + key resolution
(`settings.openrouter_api_key` else `config.openrouter_api_key`). The call is the same
Bearer POST to `{ai_endpoint}/chat/completions` as `todos/query.rs::call_ai` — **one-shot,
not streaming**. Use `temperature:0`. Do NOT force `response_format:json_object` (some
openrouter models reject it); parse the ops out of the content robustly (strip ``` fences,
take the first balanced `{...}`).

## Op schema (the model's output)

```json
{"ops":[
  {"op":"replace","find":"<verbatim unique substring of the note>","with":"<new text>"},
  {"op":"insert_after","find":"<verbatim unique substring of the note>","text":"<inserted right after it>"},
  {"op":"append","text":"<added at the very end>"}
]}
```

### System prompt (validated)

> You edit a note by emitting precise edit operations, acting as a cursor. You get the
> CURRENT NOTE and an INSTRUCTION. Emit ALL operations needed to FULLY satisfy the
> instruction. Respond with ONLY this JSON, nothing else (no prose, no code fences):
> `{"ops":[ ... ]}`. Op types: `{"op":"replace","find":"…","with":"…"}`,
> `{"op":"insert_after","find":"…","text":"…"}`, `{"op":"append","text":"…"}`.
> Rules: copy each `find` VERBATIM from the note and it MUST occur EXACTLY ONCE; emit every
> op needed; **any text you insert must include its own leading/trailing newlines where a
> new line is intended**; keep edits minimal; valid JSON only; keep reasoning very short.

User message: `CURRENT NOTE:\n{body_without_command_line}\n\nINSTRUCTION: {instruction}`

## Flow (server-driven)

1. **Detect.** The only server hook that sees committed note bodies is `flush_room`
   (`crates/server/src/collab/persist.rs:110`, debounced ~5s). Scan the flushed body for a
   line whose trimmed text ends with `#AI!`. If found AND no AI run is already in-flight for
   this note → **`tokio::spawn` the orchestration task and return immediately** (never block
   the flush on an LLM call).
2. **Guard.** Keep an in-flight set of note ids (e.g. a `DashMap`/`Mutex<HashSet>` on
   `AppState` or in the ai_command module). Insert on start, remove on finish. Skip if
   already present. Loop-safety is automatic because step 3 removes the marker.
3. **Lift the command.** Instruction = the command line minus the trailing `#AI!` (and
   surrounding whitespace). If the line is only `#AI!`, use the default instruction
   `"Improve and clean up this note."`. Remove the entire command line from the note via one
   `apply_text_edit` splice (so the marker is gone before anything else — no re-fire).
4. **Call the model.** One-shot, `ai_command_model`, with the prompt above over the
   post-strip body. Parse + collect the ops.
5. **Apply, paced, as a cursor.** `state.rooms.get_or_create(note_id, …)` ONCE, then for
   each op IN ORDER:
   - Re-read current text (`room.snapshot_text()`); **validate the anchor**: `find` must
     occur EXACTLY ONCE in the *current* text — if 0 or >1, **skip this op** (log it).
   - Move the AI cursor: publish an awareness entry for a synthetic "AI" client (user name
     `"AI"`, a distinct color, cursor at the op's offset) and broadcast it.
   - Splice under `room.lock_awareness()` → `doc.get_or_insert_text("content")` →
     `text.remove_range`/`text.insert` (replace), or `text.insert` after the anchor
     (insert_after), or at end (append) — exactly the mechanics of `apply_text_edit`
     (`collab/write.rs:31-65`): capture the pre-edit state vector, encode the delta, drop the
     awareness guard, then `room.broadcast_frame(0, …)` + `room.mark_dirty()`.
   - `tokio::time::sleep(~70ms)` between ops for the visible "watch it type" effect.
   - **Never hold the awareness `std::Mutex` across an `.await`** (the LLM call already
     finished in step 4; the sleep happens with the guard dropped).
6. **Finish.** Clear the "AI" awareness entry (broadcast its removal), `release` the room
   handle, remove the in-flight guard. The normal debounced flush persists the result.
7. **Errors.** No key/model, LLM error, or unparseable ops → append one line to the note
   like `\n> ⚠️ #AI! couldn't run: <short reason>` via `apply_text_edit`, log, and bail
   cleanly (marker already stripped). Caps: ignore notes over ~20 KB, cap at ~40 ops, cap
   model `max_tokens` (~2000).

## Key existing APIs to reuse (do not reinvent)

- `collab/write.rs::apply_text_edit` (`:31`) — the per-op splice+broadcast model; and
  `edit_note_through_room` (`:98`) for the one-shot marker-strip / error-append (it
  flushes+releases, fine for single edits — but for the paced loop get_or_create ONCE and
  loop raw `apply_text_edit`-style splices, do NOT call edit_note_through_room per op).
- `collab/room.rs`: `RoomRegistry::get_or_create` (`:195`) / `get` (`:302`) / `release`
  (`:245`); `Room::lock_awareness` (`:117`), `broadcast_frame` (`:123`), `mark_dirty`
  (`:129`), `snapshot_text` (`:138`); `CONTENT_FIELD = "content"` (`:45`).
- Awareness publishing for the "AI" cursor: craft a y-protocol awareness update and
  broadcast it like the sync frames; mirror how incoming awareness is handled in
  `collab/ws.rs` (`process_frame`, `:380`) and the client's awareness use. If publishing a
  precise remote caret proves fiddly, ship v1 with just the live text edits (guaranteed via
  broadcast) + an awareness *flag* (name "AI" present) and refine the caret later.
- AI call + key/model/endpoint resolution: `todos/query.rs::call_ai` (`:202`) + the
  resolution in `query_todos` (`:146-187`); settings in `settings/store.rs`.
- Detection hook: `collab/persist.rs::flush_room` (`:110`); note-id→room via `state.rooms`.

## Settings (`ai_command_model`)

Mirror the existing `openrouter_model` field exactly: add `ai_command_model: String` to
`UserSettings` (frontmatter key `ai_command_model`, default
`openrouter/google/gemini-3.1-flash-lite`); expose in `SettingsResponse`; accept in
`PutSettingsRequest`. In the Settings UI, add a small control under `AI · LiteLLM`
("Command model — used by `#AI!` inline edits") reusing the same searchable model picker
(or a plain text field) + the `/api/ai/models` list.

## Frontend

The **live text edits require no client change** — they arrive as normal CRDT sync frames
and render in CodeMirror automatically. If the server publishes the "AI" awareness entry,
y-codemirror renders its remote cursor like a human collaborator's (style/label it "AI").
Optionally add a subtle "AI editing…" affordance keyed off that awareness entry. No hard
lock — the user may co-edit; the CRDT merges.

## Constraints (CI)

Panic-safety clippy gate `-D warnings` (no unwrap/expect/panic/indexing in prod);
`cargo fmt`; `cargo test` green (add tests: op parse/validate incl. verbatim-unique anchor
check and skip-on-ambiguous; instruction extraction incl. `#AI!`-only default; apply
engine on a sample doc). `thiserror`/`AppResult`. No new crates. No DB migration. rustc 1.94.
Build cargo inside `nix-toolbox-44`.

## Build decomposition (coordinator note)

- **Agent 1 (backend, opus):** the whole `ai_command` module — op types + parse/validate +
  the paced apply-to-room engine + the AI-command call + the `flush_room` detection hook +
  the in-flight guard + the "AI" awareness presence + error handling + unit tests. Owns all
  collab-mutation code (tightly coupled — one owner).
- **Agent 2 (settings, sonnet):** `ai_command_model` field (store.rs + routes.rs) + the
  Settings UI control.
- **Agent 3 (frontend, sonnet):** style/label the "AI" remote cursor + optional "AI
  editing…" affordance; verify live edits render.
