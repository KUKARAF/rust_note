# `#AI!` v2 — streamed generation, animated apply, and live feedback (proposal)

Status: proposal / design. v1 (`docs/ai-command.md`) shipped one-shot generation + a
70ms-paced apply + a synthetic "AI" awareness presence. This doc proposes v2 to fix the
four user-reported problems and lays out a phased plan.

## TL;DR recommendation

The felt problem is **latency with no feedback** and **an edit that lands as one bulk
blob**, not a lack of streaming per se. So:

- **Do the cheap, high-impact fixes first (Phase 1), without any SSE work.** They solve 3
  of the 4 complaints: trigger detection in the live room on a short debounce (not the ~5s
  git flush), publish the "AI" presence *immediately* on detection and keep it up for the
  whole run (fixes the indicator), and make the *apply* genuinely animated by typing each
  op's new text char-by-char with a moving cursor instead of one big splice.
- **Then add real SSE streaming (Phase 2)** so first visible text appears ~1s sooner and
  the note fills progressively during generation. Recommended apply strategy:
  **option (c) for Phase 1 (animate the one-shot apply), then option (a) for Phase 2
  (stream JSONL ops, apply each the moment it completes)** — keep the surgical anchored-op
  model; do NOT free-stream raw replacement text into the doc.
- **Add accept/reject affordances last (Phase 3)**, leaning on the existing Yjs
  `UndoManager`.

---

## 1. Current implementation — confirmed root causes

### RC1 — "nothing happens for a while" after typing `#AI!` (CONFIRMED)

Detection is wired only into the debounced git-persistence hook.
`collab/persist.rs::flush_room` calls `crate::ai_command::maybe_spawn(room, state, &text)`
at **`persist.rs:122`**, and `flush_room` only runs after the debounce in
`spawn_persistence` — `DEBOUNCE = 5s` (**`persist.rs:37`**), capped by `MAX_INTERVAL = 30s`
(**`persist.rs:40`**). So the marker isn't even *seen* until ≥5s of typing silence, before
the LLM call has begun. `maybe_spawn` itself is cheap (`ops::locate_command` scan +
in-flight guard, **`mod.rs:76-92`**) — the latency is entirely the 5s debounce gate.

### RC2 — no animation during generation (CONFIRMED)

Nothing is published between detection and the first applied op. In `run`
(**`mod.rs:95-128`**) the sequence is: strip the marker (**`mod.rs:117`**) →
`generate_and_apply` → `llm::call_model` (**`mod.rs:163` / `llm.rs:49`**, a blocking
one-shot POST with a 30s timeout, **`llm.rs:72-84`**) → only then `apply::apply_ops`. The
"AI" awareness presence is first published *inside* `apply_ops`, at
**`apply.rs:63-64`** — i.e. after the whole LLM round-trip. During the user's entire wait
there is zero presence and zero animation.

### RC3 — the edit landed as one bulk "append → remove → append" (CONFIRMED)

Two compounding causes:

1. **Whole-note cleanup ⇒ ~1 giant `replace` op.** The bare-`#AI!` default instruction is
   `"Improve and clean up this note."` (**`ops.rs:15`**). A cleanup rewrites throughout, so
   the model returns effectively one `replace` spanning most of the note. `apply_ops`
   (**`apply.rs:43-77`**) paces *between* ops with `PACE = 70ms` (**`apply.rs:34`**) — with
   a single op there is nothing to pace.
2. **Each op is one minimal splice, and a big replace's minimal splice is still a big
   remove+insert.** `apply_text_edit` (**`write.rs:31-65`**) computes the longest common
   prefix/suffix via `byte_diff` (**`write.rs:70-89`**) and emits one `remove_range` + one
   `insert` for the differing middle (**`write.rs:50-56`**). For a throughout-rewrite the
   middle is large → the "remove then append" the user saw.
3. **The marker strip is a separate splice** done before generation
   (**`mod.rs:117`** → `ops::strip_command_line`), so the user first sees the command line
   vanish, then a long pause, then the bulk replace — two disjoint jumps, no sense of
   progress.

### RC4 — the "AI editing…" indicator did not show (CONFIRMED — it's a ~70ms flash)

The wiring is actually correct end-to-end; the problem is **duration**, not a broken path:

- Server publishes presence `{"user":{"name":"AI","color":"#d9480f"}, "aiCursorOffset":N}`
  (**`apply.rs:89-95`**) by applying an `AwarenessUpdate` for the sentinel client
  `AI_CLIENT_ID = 0x00A1_C0DE` (**`apply.rs:28`**) to the room awareness and broadcasting it
  with origin 0 (**`apply.rs:100-119`**). Origin 0 is never a real conn id (conn ids start
  at 1, `room.rs:94`), so every socket relays it (`ws.rs:303-312`).
- Client applies it to its y-protocols `Awareness`; `refreshPeers` iterates
  `awareness.getStates()` and sets `aiEditing = true` when any state has
  `user.name === 'AI'` (**`+page.svelte:206-231`**), which drives the header chip
  (**`+page.svelte:469-474`**).

But presence is only published *inside* `apply_ops`: first entry at the first op
(**`apply.rs:63-64`**), retracted (`"null"`, clock+1) immediately at the end
(**`apply.rs:72-74`**). With RC3's single-op apply, the lifetime is one op + one `PACE`
(~70ms) before retraction — an imperceptible flash, and only *after* the long silent wait
of RC1+RC2. Net effect: "it never showed."

Secondary gap: the presence carries no `cursor: {anchor, head}` RelativePosition, only a
byte `aiCursorOffset`, so **y-codemirror renders no in-editor AI caret at all** — the
`TODO(ai-command)` at **`apply.rs:84-88`** is still open. The only affordance is the header
chip.

---

## 2. Streaming the response — research & recommendation

### 2a. The SSE wire format (LiteLLM / OpenAI-compatible)

Add `"stream": true` to the request body (alongside the current `model`/`messages`/
`temperature`/`max_tokens` in `llm.rs:59-67`). The endpoint then responds `Content-Type:
text/event-stream` with line-framed events:

```
data: {"choices":[{"delta":{"content":"{\"ops\""},"index":0}]}
data: {"choices":[{"delta":{"content":":[{\"op\""},"index":0}]}
...
data: {"choices":[{"delta":{},"finish_reason":"stop","index":0}]}
data: [DONE]
```

Rules that matter (confirmed against OpenAI-compatible SSE docs):
- Concatenate `choices[0].delta.content` across chunks; it's absent/empty on the role-only
  first chunk and the final `finish_reason` chunk — skip those.
- The terminal sentinel is the literal line `data: [DONE]`.
- Chunks do **not** align to line or token boundaries; a single SSE `data:` line can carry
  a partial JSON token. Buffer and split on `\n`, keeping the trailing partial line.

### 2b. reqwest config needed (CONFIRMED missing)

`reqwest` is declared `default-features = false, features = ["json", "rustls-tls"]`
(workspace `Cargo.toml:31`; re-exported in `crates/server/Cargo.toml:30`). `Response::
bytes_stream()` lives behind the **`stream`** feature, which is **not** enabled — so today
streaming literally won't compile. Phase 2 needs:

- `reqwest` feature `"stream"` added.
- A stream combinator import — there is **no `futures`/`futures-util`/`tokio-stream` crate
  in the tree today** (`grep` across all `Cargo.toml` finds none). Add `futures-util`
  (for `StreamExt::next`) *or* `tokio-stream`. This contradicts v1's "no new deps" rule, so
  it's a deliberate Phase-2 cost.
- Keep the SSE parse hand-rolled (don't pull an `eventsource` crate): accumulate
  `bytes_stream()` chunks into a `String` buffer, split on `\n`, strip the `data: ` prefix,
  stop on `[DONE]`, `serde_json`-parse each payload for `delta.content`.
- Timeout: the current `.timeout(Duration::from_secs(30))` (`llm.rs:74`) is a whole-request
  deadline and is fine for streaming too, but consider swapping to a *read*/idle timeout so
  a long-but-progressing generation isn't killed.

### 2c. The hard part — applying a stream to the Yjs CRDT

The CRDT mechanics are already solved and must be reused: `apply_text_edit`
(`write.rs:31-65`) does lock → `byte_diff` → `remove_range`/`insert` → encode delta → drop
guard → `broadcast_frame(0,…)` → `mark_dirty`. The one hard constraint: the awareness
`std::sync::Mutex` must **never be held across `.await`** (`room.rs:103-119`,
`apply.rs:7-9`). `apply_text_edit` is fully synchronous and takes the guard internally, so
the pattern is: `await` the next stream chunk → parse → call `apply_text_edit` (sync) →
`sleep` (guard already dropped). Three options evaluated:

**Option (a) — stream ops as JSONL / incrementally-parsed, apply each on completion.**
Prompt the model to emit one op per line (JSONL) instead of a `{"ops":[…]}` envelope.
Maintain the SSE content buffer; whenever a newline completes a line, `parse_ops`-style
decode that one op and immediately validate+apply it (moving the AI cursor to it).
- Pros: keeps the surgical anchored model intact; each op still validated against *current*
  text before applying (`ops.rs:33-66`), so stale/ambiguous anchors still skip safely; the
  user watches ops land one-by-one *as they generate* — real progress. Natural fit with the
  existing `apply_ops` loop (just feed it from a channel instead of a `Vec`).
- Cons: needs a prompt change (JSONL, not the current envelope — `llm.rs:18`); partial-JSON
  robustness (a half-received line must not parse). Mitigate by only parsing a line once its
  newline arrives. Small/cheap models (default `gemini-3.1-flash-lite`) are reliable at
  line-delimited JSON at `temperature:0`.
- **This is the recommended Phase-2 target.**

**Option (b) — two-phase "type it out": AI cursor streams replacement text token-by-token
into the target region.** First get the anchors (which region to edit), then stream the new
text into that region grapheme-by-grapheme.
- Pros: the most "alive" feel — literally watching it type.
- Cons: biggest redesign. You must know the full target span *before* you can stream into
  it (so you still need the op structure first), and inserting mid-note while the model is
  mid-thought fights the "validate against current text" invariant. Highest complexity for a
  marginal feel gain over (c)+(a). **Not recommended** as the primary mechanism, but note
  that (c) already delivers 90% of this feel with none of the risk.

**Option (c) — keep one-shot generation, make the APPLY genuinely animated.** No SSE at
all. In `apply_ops`, replace the single big splice per op with **char/grapheme-chunked
splices**: for a `replace`/`insert`, first move the AI cursor to the offset, optionally
delete the old span in one splice, then insert the new text in N-grapheme increments
(e.g. 2-4 graphemes every ~20-40ms), advancing the cursor offset each tick. Each tick is a
normal `apply_text_edit` call (sync) followed by a `sleep` with the guard dropped.
- Pros: **no new deps, no prompt change, no SSE** — solves RC3's "felt bulk" entirely and
  (because presence stays up the whole time) RC2/RC4. Reuses all existing CRDT plumbing.
  Lowest risk, shippable immediately.
- Cons: doesn't reduce time-to-first-edit (still waits for the full LLM response before any
  animation). That's what Phase 2 (a) adds.
- **This is the recommended Phase-1 apply engine.**

**Recommendation:** (c) now, (a) next. Together they give progressive generation *and* a
typed-out feel while preserving the validated anchored-op safety model. Skip (b).

---

## 3. UX affordances for inline AI editing

### Patterns studied (what good tools do)

- **Notion AI / Google Docs "Help me write":** the instant you invoke, the block switches
  to a *generating* state — a shimmering/pulsing placeholder and a label ("Writing…"/
  "Thinking…") — then text streams in token-by-token, and it finishes in a pending state
  with explicit **Accept / Discard / Try again / Keep** actions before the text is
  committed. Recognition is immediate (on Enter/invoke), not debounced.
- **Cursor (inline edit, Cmd+K):** immediate armed input, then a streamed **inline diff**
  (green additions / red deletions) rendered in place with a shimmer on pending regions, and
  Accept/Reject per-hunk or for the whole edit. The key lesson: *show the work happening in
  the document*, with a clearly non-final (pending) visual state.
- **Linear AI:** minimal, fast; a small spinner/label on the triggering control and a tight
  streamed result — reinforces that a lightweight "working" chip beats a modal for an inline
  action.
- **Raycast AI:** a pulsing/animated cursor and streamed text with a persistent "AI is
  working" affordance in the chrome; cancel is always available.

Common thread: **(1) immediate recognition on invoke, (2) an unmistakable working
animation, (3) streamed text in-place, (4) an explicit accept/reject (or trivial undo).**

### Concrete proposal for `#AI!`

**(a) Immediate recognition — fix the trigger latency (RC1).** Detect the completed `#AI!`
line in the *live room*, not at the 5s git flush. Options, in order of preference:
1. **Short dedicated debounce (~400-700ms) on the marker.** Hook detection into the
   inbound-edit path (`ws.rs::process_frame`, after an applied `Update` at `ws.rs:425-429`,
   or a new lightweight observer on the room doc) with its own short timer, independent of
   the 5s persistence `DEBOUNCE`. This keeps the "type-and-it-just-goes" feel.
2. **Enter-to-confirm:** only fire when the `#AI!` line is *terminated* by a newline (the
   user pressed Enter), which is an explicit, intentional trigger and avoids firing on a
   half-typed `#AI`. Cheap to detect server-side (marker line is followed by `\n`).
   **Recommended** combined with a tiny debounce — explicit and prompt.
The git-flush hook stays as a *backstop* (covers a note closed before the live trigger
fired).

**(b) Working animation — the moment it's armed.** On detection, immediately (before the
LLM call) publish the "AI" awareness presence and keep it up for the whole run (see §4),
so the header chip "AI editing…" (`+page.svelte:469-474`, already built, uses `PulsingDot`)
lights up at once and stays lit through generation. Add an in-editor cue: either (i) a
pulsing **AI caret** at the command-line location by encoding a real `cursor:{anchor,head}`
RelativePosition so y-codemirror renders it (closes the `apply.rs:84` TODO), or (ii) a
lighter-weight decoration. Start with the chip (free) + Phase-2 caret.

**(c) Progressive / animated edits.** Phase 1: grapheme-chunked typing per op (§2c option
c). Phase 2: ops stream in and apply as generated (option a). Either way the AI cursor
moves to each edit site as it happens — the presence `aiCursorOffset` already carries the
site (`apply.rs:89-95`); render it as a real caret in Phase 2.

**(d) Accept / reject / undo.** The Yjs `UndoManager` already exists
(`collabProvider.ts:119`, bound in CodeMirror at `CodeMirrorEditor.svelte:251-253`), but it
tracks **local** edits only — AI edits arrive as remote updates and won't be in the local
undo stack. Phase 3 options: (i) simplest — a transient "AI edited · Undo" toast that calls
a dedicated revert (snapshot the pre-run text, offer a one-click restore via
`apply_text_edit`); (ii) richer — a pending/accept overlay à la Cursor. Recommend (i) first.
A hard lock is explicitly *not* wanted (the CRDT merges concurrent human edits) — keep it
non-blocking.

---

## 4. Phased plan (what to build, effort, files)

### Phase 1 — feedback + animation + fast trigger (no SSE, no new deps) — ~1-1.5 days

Highest impact per effort; solves RC1, RC2, RC3, RC4.

1. **Publish AI presence on detection, hold it for the whole run.** Move the first
   `broadcast_ai_state(...)` out of `apply_ops` to the top of `run` (right after the marker
   strip), and the retraction to the end of `run` (not the end of `apply_ops`). Keep per-op
   cursor-offset updates during apply.
   - Files: `ai_command/mod.rs` (`run`, ~`:95-128`), `ai_command/apply.rs`
     (`apply_ops`/`broadcast_ai_state`, `:43-119`).
   - Fixes RC2 + RC4.
2. **Animate the apply (option c).** In `apply_ops`, replace the single splice per op with
   grapheme-chunked insertion (and a one-shot delete of the replaced span), ~2-4 graphemes
   per ~20-40ms tick, moving the cursor each tick. Keep the mutex-not-across-await rule:
   `apply_text_edit` (sync) then `sleep`.
   - Files: `ai_command/apply.rs` (`:43-77`); reuses `collab/write.rs::apply_text_edit`.
     Add `unicode-segmentation` *or* iterate `char_indices` (no new dep) for grapheme-safe
     chunking.
   - Fixes RC3.
3. **Fast trigger in the live room.** Add a short-debounce detector on the inbound edit path
   (preferred: fire when the `#AI!` line is newline-terminated), keeping the existing
   `flush_room` call as a backstop. Reuse the `INFLIGHT` guard (`mod.rs:35-69`) so the live
   trigger and the flush backstop can't double-fire.
   - Files: `collab/ws.rs` (`process_frame`, `:425-429`) or a new room-doc observer +
     timer; `ai_command/mod.rs` (`maybe_spawn`); `collab/persist.rs:122` left as backstop.
   - Fixes RC1.
4. **(Optional, cheap) real AI caret.** Encode `cursor:{anchor,head}` as Yjs
   RelativePosition JSON in `presence_json` so y-codemirror draws the caret (closes
   `apply.rs:84` TODO). Can slip to Phase 2.

### Phase 2 — real SSE streaming + streamed ops (option a) — ~2-3 days

Reduces time-to-first-edit and fills the note progressively during generation.

1. **Enable streaming transport.** Add `reqwest` feature `"stream"` + `futures-util`
   (`Cargo.toml`). Add `"stream": true` to the request body.
   - Files: workspace `Cargo.toml:31`, `crates/server/Cargo.toml:30`, `ai_command/llm.rs`.
2. **SSE reader.** New `llm::call_model_streaming` returning a `Stream`/`mpsc` of content
   deltas: `resp.bytes_stream()` → buffer → split `\n` → strip `data: ` → stop on `[DONE]`
   → parse `choices[0].delta.content`.
   - Files: `ai_command/llm.rs` (new fn; keep the one-shot as fallback).
3. **Switch the op schema to JSONL + incremental parse.** Update `SYSTEM_PROMPT`
   (`llm.rs:18`) to "one op per line, no envelope"; add a streaming parser in `ops.rs` that
   decodes a line once its newline arrives (reuse `RawOp`/`into_op`, `ops.rs:93-121`).
   Feed completed ops into the existing validate+apply loop.
   - Files: `ai_command/ops.rs`, `ai_command/mod.rs` (`generate_and_apply`), `apply.rs`.
4. **Cancellation/robustness.** Idle timeout instead of a hard 30s; on parse failure of a
   line, skip it (the anchored-op validation already makes a dropped op safe).

### Phase 3 — accept/reject affordances — ~1-2 days

1. Snapshot pre-run text; after a run, broadcast a lightweight signal (reuse the awareness
   channel or a new field) so the client shows an "AI edited · Undo" toast that reverts via
   one `apply_text_edit`.
   - Files: `ai_command/mod.rs` (snapshot + revert entry point), `web/.../+page.svelte`
     (toast, near the existing `aiEditing` chip), `web/.../collabProvider.ts` if a new
     awareness field is used.
2. (Stretch) Cursor-style pending/diff overlay in CodeMirror — larger effort, defer unless
   validated as needed.

### Cross-cutting

- Keep the `std::sync::Mutex`-not-across-await invariant in every new apply path
  (`room.rs:103-119`).
- CI gates unchanged: panic-safety clippy `-D warnings` (no unwrap/expect/panic/indexing in
  prod), `cargo fmt`, `cargo test`. Add tests for: grapheme-chunked apply, the live-room
  trigger debounce + INFLIGHT interplay, and (Phase 2) the SSE line parser + JSONL op parse.
- Phase 2 is the only phase that adds crates (`reqwest "stream"`, `futures-util`) — flag it
  explicitly since v1 was "no new deps".
