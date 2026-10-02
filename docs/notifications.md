# Pipeline overdue → priority-notify digest (data contract)

Push a **daily digest** of overdue pipeline items to the user's devices via their
self-hosted **priority-notify** server (`notifications.osmosis.page`). Everything is
driven from per-user Settings; no env editing, no new crates, no DB migration.

## priority-notify API (the target)

- Send: `POST {endpoint}/api/notifications/` with header `Authorization: Bearer <token>`,
  JSON body `{ "title": str(<=500), "message": str|null, "priority": "low"|"medium"|"high"|"critical", "source": str|null }`.
- Returns 201 on success. There is **no idempotency / dedup** server-side and **no url
  field** — so the deep-link goes in `message`, and we avoid spam by sending **one digest
  per scheduled fire** (not one-per-item).
- The token is a priority-notify API token the user mints in priority-notify's own UI.

## Settings (per-user, stored in `_settings/{user_id}/main`)

Extend `UserSettings` (`crates/server/src/settings/store.rs`) following the EXACT pattern
of `openrouter_api_key` — secrets stored server-side, parsed tolerantly from flat
frontmatter, NEVER returned to the client (exposed only as a `has_*` bool). New fields +
frontmatter keys + defaults:

| Field | frontmatter key | default | notes |
|---|---|---|---|
| `notify_enabled: bool` | `notify_enabled` | `false` | parse "true"/"false" tolerantly |
| `notify_endpoint: String` | `notify_endpoint` | `https://notifications.osmosis.page` | base URL; POST path `/api/notifications/` appended |
| `notify_token: String` | `notify_token` | `""` | **write-only secret** (like openrouter_api_key) |
| `notify_priority: String` | `notify_priority` | `high` | one of low/medium/high/critical |
| `notify_scope: String` | `notify_scope` | `both` | one of ours/theirs/both |
| `notify_schedule: String` | `notify_schedule` | `FREQ=DAILY;BYHOUR=8;BYMINUTE=0` | an RRULE (iCal); evaluated in `crate::stats::DEFAULT_TZ` (Europe/Warsaw) |
| `notify_last_sent: String` | `notify_last_sent` | `""` | internal bookkeeping (RFC3339); set by the scheduler after each fire; NOT user-editable, NOT returned to client |

### `/api/settings` changes (`crates/server/src/settings/routes.rs`)

- `SettingsResponse` gains: `notify_enabled`, `notify_endpoint`, `notify_priority`,
  `notify_scope`, `notify_schedule`, and `has_notify_token: bool`
  (`= !notify_token.is_empty()`). Do NOT return `notify_token` or `notify_last_sent`.
- `PutSettingsRequest` gains optional: `notify_enabled`, `notify_endpoint`,
  `notify_priority`, `notify_scope`, `notify_schedule`, `notify_token` (write-only).
  Apply each via `fm.set(...)` exactly like `openrouter_api_key`/`openrouter_model`.
- **Validate on PUT** (reject with `AppError::BadRequest`): `notify_priority` ∈
  {low,medium,high,critical}; `notify_scope` ∈ {ours,theirs,both}; `notify_schedule`
  parses as a valid RRULE via the `rrule` crate; `notify_endpoint` parses as an http(s) URL.

## Reusable pipeline collection

Factor the walk+parse+overdue logic currently inside `crates/server/src/pipeline/routes.rs`
into a callable helper the scheduler can call directly (no self-HTTP), e.g.
`pub async fn collect_pipeline(state: &AppState, user_id: &str, kind: Option<Kind>, include_closed: bool) -> AppResult<Vec<PipelineEntry>>`, and have the existing route call it.
`PipelineEntry` already carries `overdue`. The scheduler filters:
`overdue == true && ball matches scope` where scope `ours`→`Ball::Ours`, `theirs`→`Ball::Theirs`, `both`→either (never `Ball::None`).

## The scheduler (new module `crates/server/src/notify/`)

NO background task exists today — add one. Spawn it from `main.rs` via `tokio::spawn`
AFTER `AppState` is built (guard: only when not in a unit-test context; it's fine to run
in dev). Loop pattern:

- Tick every **60s** (`tokio::time::interval`). On each tick, for **each user**
  (`SELECT id FROM users`):
  1. Load settings (`settings::store::load_or_bootstrap`). Skip if `!notify_enabled` or
     `notify_token` empty.
  2. Decide if the RRULE fired: compute whether `notify_schedule` has an occurrence in the
     half-open window `(last, now]` where `last = notify_last_sent` (or `now - 25h` if
     empty, so a fresh config doesn't instantly replay old occurrences), in Europe/Warsaw.
     Use the `rrule` crate (already a dep). If no occurrence → skip.
  3. `collect_pipeline` → filter overdue-by-scope. Build ONE digest (below).
  4. If there ARE overdue items → POST to priority-notify (reqwest, already a dep).
     If there are NONE → send nothing (silence is good).
  5. **Always** update `notify_last_sent = now` afterwards (success OR empty OR POST
     failure) so an occurrence fires at most once; log POST failures with `tracing` and
     move on. Per-user errors must never crash the loop (catch + log).

Writing `notify_last_sent` back: update the settings note the same way `put_settings`
persists fields (load note → `fm.set("notify_last_sent", ...)` → save). Keep it minimal.

### Digest payload

- `title`: if 1 item → `⏰ {company} — {next_action}`; if N>1 → `⏰ {N} items need your move`.
- `message`: one bullet per item, most-overdue first:
  `• {company}{ — role}{ — Nd overdue} · {next_action}` and a trailing link line
  `→ https://notes.osmosis.page/pipeline`. (ours-overdue reads "your move is late";
  theirs-overdue reads "follow up — silent since due".) Compute `Nd overdue` from
  `now - expected_at`.
- `priority`: the user's `notify_priority`. `source`: `"pipeline"`.

### Test trigger (powers the "Send test" button)

`POST /api/settings/notify-test` (authed, current user): run the digest NOW, ignoring
schedule AND `notify_last_sent`, and WITHOUT updating `notify_last_sent`. Returns
`{ "sent": bool, "count": n, "title": str, "message": str }`. If `notify_token` empty →
`AppError::BadRequest("configure the priority-notify API key first")`. If 0 overdue →
`sent=false, count=0` and a title like `Nothing overdue` (do not POST). Register the route
in the settings router.

## Frontend: NOTIFICATIONS section in Settings (`web/src/routes/settings/+page.svelte`)

Add a section directly under `AI · OPENROUTER`, matching its exact terminal grammar
(uppercase header `NOTIFICATIONS · PRIORITY-NOTIFY`, a muted description line, the same
pill/field/`$lib/design` components). Fetch/update via `GET`/`PUT /api/settings`.

- `ENABLED` — a pill toggle bound to `notify_enabled`.
- `SCOPE` — three pills OURS / THEIRS / BOTH bound to `notify_scope`.
- `PRIORITY` — four pills LOW / MEDIUM / HIGH / CRITICAL bound to `notify_priority`.
- `SCHEDULE` — a small preset selector mapping friendly labels → RRULE strings:
  - `Daily at 08:00` → `FREQ=DAILY;BYHOUR=8;BYMINUTE=0`
  - `Daily at 18:00` → `FREQ=DAILY;BYHOUR=18;BYMINUTE=0`
  - `Weekday mornings` → `FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR;BYHOUR=8;BYMINUTE=0`
  - `Custom…` → reveal a text input for a raw RRULE.
  Show the active RRULE string under the picker. Persist as `notify_schedule`.
- `ENDPOINT` — a text field bound to `notify_endpoint` (default shown).
- `API KEY` — a write-only field + SAVE button, status line "key set" / "no key set"
  driven by `has_notify_token`, exactly like the OpenRouter key block.
- `SEND TEST` — a button → `POST /api/settings/notify-test`; show the returned result
  inline (e.g. "Sent: 3 items" / "Nothing overdue" / an error). Disable while no key set.

## Hard constraints (CI enforces)

- Panic-safety clippy gate `deny` in prod code (no unwrap/expect/panic/indexing); combinators/`?`/`.get()`. `cargo clippy -p server -p rust-note-core --lib --bins -- -D warnings` must be clean.
- `cargo fmt` clean, `cargo test --workspace` green (add unit tests: settings parse/round-trip of the new fields incl. secret-never-returned; scope→ball filtering; RRULE window decision; digest formatting).
- Errors via `thiserror` `AppError`/`AppResult`; never leak internals. Timestamps RFC3339 via `time` (`acl.rs:now_rfc3339`). RRULE + reqwest + time are already workspace deps — add NO new crates. No DB migration.
- Settings secrets (`notify_token`) must never appear in any JSON response or log line.
- rustc 1.94 target.
