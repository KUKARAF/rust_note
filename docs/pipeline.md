# Pipeline tracker — data contract

A unified tracker for two kinds of opportunity that share the same
"whose-court-is-the-ball-in" mechanics:

- **lead** — a contact Rafał could do a gig for (freelance / B2B).
- **application** — Rafał applied to a company as a candidate (the job-search funnel).

House style (see `tasks.rs` / `stats.rs`): each item is **one git-backed Markdown
note** with typed fields in flat frontmatter, a freeform **timeline** in the body,
and git history as the event log. NO SQLite table, NO migration. A typed parser in
`crates/core` reads the frontmatter; the server aggregates by walking notes (exactly
like `GET /api/todos`); the web view clones the todo board; MCP tools let an LLM
maintain it.

## Namespace & id

One note per item under `pipeline/`:

- `pipeline/affirm-senior-swe-backend`
- `pipeline/acme-consulting-rag-gig`

Slug = `pipeline/<company-slug>[-<short-role-slug>]`, lowercase, hyphenated, using the
existing `slugify_note_id` rules.

## Frontmatter schema (flat `key: value` strings only)

The core `Frontmatter` parser round-trips flat string pairs only — no YAML lists or
nesting. Lists are **comma-delimited strings**; timestamps are **RFC3339 TEXT**
(`OffsetDateTime::now_utc().format(&Rfc3339)`, the convention at `acl.rs:238`); dates
are `YYYY-MM-DD`. Every field except `kind` + `company` is optional and absence must
never error (follow the never-panic style of `read_stats`/`parse_tasks`).

| Key | Type (as text) | Meaning |
| --- | --- | --- |
| `kind` | `lead` \| `application` | **required** — which tracker this is |
| `company` | string | **required** — company / contact org |
| `role` | string | role applied for, or the gig / what-for |
| `stage` | string (vocab below) | pipeline stage; terminal stages drive "closed" |
| `ball` | `ours` \| `theirs` \| `none` | whose court the next move is in; `none` = closed/dormant |
| `expected_at` | RFC3339 | deadline the ball should move by. ball=ours → our self-SLA (act by); ball=theirs → chase date (nudge after) |
| `next_action` | string | the concrete thing the ball represents ("send CV", "reply to Paulina") |
| `contact` | comma-list | `Name <email> (role)` entries, comma-separated |
| `priority` | integer 1–5 | higher = chase harder; optional |
| `closed_reason` | string | only when stage is terminal — "ghosted", "below floor", "accepted elsewhere" |

### `stage` vocabulary (not enforced by the parser; documented convention)

- application: `prospect` → `applied` → `screening` → `interview` → `offer` → `closed`
- lead: `prospect` → `talking` → `proposal` → `won` → `closed`
- terminal stages (→ treated as closed, ball should be `none`): `closed`, `won`, `lost`, `rejected`, `withdrawn`, `accepted`

## Body = timeline (the event log)

Below the frontmatter, a human-readable reverse-chronological log. Each MCP
`log_pipeline_event` call prepends a dated line here (and optionally sets
`ball` / `expected_at` / `stage`). Git history preserves every past state — no
events table needed.

```markdown
---
kind: application
company: Affirm
role: Senior SWE, Back-end (Payment Experiences)
stage: interview
ball: theirs
expected_at: 2026-10-08T12:00:00Z
next_action: await Aakash's reschedule after today's no-show
contact: Paulina Syberska <paulina.syberska@affirm.com> (coordinator)
priority: 4
---
# Affirm — Senior SWE Back-end

## Timeline
- 2026-10-01 — HackerRank w/ Aakash Gandhi (Staff SWE): no-show; emailed him + CC Paulina. ball → theirs.
- 2026-09-22 — Recruiter screen w/ Julia Araźny → advanced.
- 2026-09-16 — Applied (referral).
```

## Rust type (in `crates/core/src/leads.rs`)

```rust
pub enum Kind { Lead, Application }          // parse "lead"/"application"; unknown → None (skip item)
pub enum Ball { Ours, Theirs, None }         // parse ours/theirs/none; default None

pub struct PipelineItem {
    pub id: String,                          // note id, e.g. "pipeline/affirm-..."
    pub kind: Kind,
    pub company: String,
    pub role: Option<String>,
    pub stage: Option<String>,
    pub ball: Ball,
    pub expected_at: Option<OffsetDateTime>, // parsed RFC3339; unparseable → None
    pub next_action: Option<String>,
    pub contacts: Vec<String>,               // split on ',', trimmed, empties dropped
    pub priority: Option<u8>,                // parse 1..=5; out-of-range/non-int → None
    pub closed_reason: Option<String>,
    pub stage_history: Vec<StageEntry>,      // parsed "<stage>@<rfc3339>" comma-list
}
```

- Constructor `PipelineItem::from_frontmatter(id: &str, fm: &Frontmatter) -> Option<Self>`
  — `None` only when `kind` is missing/invalid or `company` is empty; every other field
  is best-effort and never errors.
- Helper `PipelineItem::is_closed(&self) -> bool` — true when `stage` is a terminal
  stage (match the terminal list above, case-insensitive) OR `ball == None`.
- `crates/core` must stay dependency-pure (no axum/server deps). `time` + `serde` are
  already workspace deps and allowed. Derive `serde::Serialize` on `PipelineItem`
  (+ the enums, serialized lowercase) so the server can return it as JSON directly.
- Unit tests in-module (`#[cfg(test)]`, where `unwrap` is allowed per `clippy.toml`).

## REST: `GET /api/pipeline`

Mirror `crates/server/src/todos/routes.rs`: walk notes whose id starts with
`pipeline/`, ACL-filter (`can_read`), prefer a live collab room's text over disk,
parse each via `PipelineItem::from_frontmatter`, drop `None`s.

- Query params: `kind` (`lead` | `application` | omitted = all), `include_closed`
  (bool, default false → hide items where `is_closed()`).
- Response: JSON array of `PipelineItem` **plus a derived** `overdue: bool`
  = `ball != none && expected_at.is_some() && now_utc() > expected_at`. Put `overdue`
  on the wire via a thin server-side response struct that flattens `PipelineItem` +
  `overdue` (don't pollute the core type with request-time derivations).
- Register in `crates/server/src/routes.rs` alongside the other `/api/*` routes;
  reuse the same auth extractor the todos route uses.

## MCP tools (in `crates/server/src/mcp/server.rs`)

Follow the 11 existing tools exactly (schemars arg structs, `require_user`, write
through `edit_note_through_room` + `authorize_write`, settings-id block already covers
`_settings/*` not `pipeline/*`):

- `list_pipeline` — args `{ kind?, include_closed? }` → same shape as `GET /api/pipeline`.
- `create_pipeline_item` — args `{ company, kind, role?, stage?, ball?, expected_at?, next_action?, contact?, priority?, note? }` → slugs an id, writes the frontmatter note (+ optional body).
- `update_pipeline_item` — args `{ id, <any frontmatter field> }` → sets/merges the given frontmatter keys, leaving others intact (use `Frontmatter::set`).
- `log_pipeline_event` — args `{ id, line, ball?, expected_at?, stage?, next_action? }` → prepends `- <today> — <line>` under the body `## Timeline`, and applies any of ball/expected_at/stage/next_action supplied.

## Web: `/pipeline` view (in `web/src/routes/pipeline/+page.svelte`)

Clone the todo board (`web/src/routes/todo/+page.svelte` + `web/src/lib/notes/todos.ts`
patterns) and reuse `$lib/design` components (`Card`, `Button`, `Input`, `Chip`,
`SectionTitle`, `Checkbox`). Fetch `GET /api/pipeline?include_closed=…`.

- **Needs attention** section first: items where `ball != 'none' && overdue`, sorted
  by how far past `expected_at` (most overdue first). This is the whole point.
- **Pipeline** section: remaining open items grouped by `stage`, sorted by `priority`
  then `expected_at`.
- A kind filter (All / Leads / Applications) and a show-closed toggle.
- Each card: company · role · stage chip · ball indicator (ours = needs-you accent,
  theirs = muted) · `next_action` · relative `expected_at` ("2d overdue", "in 3d").
- Add a `/pipeline` nav link in `web/src/routes/+layout.svelte` next to `/todo`.

## Hard constraints (CI enforces)

- Panic-safety clippy gate is `deny` in production code: no `unwrap`/`expect`/`panic`/
  `todo`/`unimplemented`/`unreachable`/`indexing_slicing`. Use combinators, `?`,
  `.get()`, `unwrap_or_default`. Tests are exempt. `cargo clippy --workspace` must pass.
- `cargo test --workspace` must pass. `cargo fmt` clean.
- Errors: `thiserror` `AppError` / `AppResult<Json<T>>` in handlers; never leak internals.
- Never write note files directly — go through `edit_note_through_room`
  (`collab/write.rs`) and call `acl::touch_updated_at` after writes.
- Targets rustc 1.94; avoid newer-stable-only lints/features.
