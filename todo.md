# TODO

## Security

- [ ] **Add CSRF protection for state-changing routes.** The backend currently
  has no CSRF defense beyond `SameSite=Lax` session cookies (`main.rs`), which
  only mitigates cross-site *navigation* cases - it does not protect against
  same-site subdomain attackers and offers nothing if the cookie policy ever
  loosens. All mutating routes (`POST`/`PUT`/`PATCH`/`DELETE` under `/api`,
  `POST /auth/logout`) are called from the SPA via `fetch` with
  `credentials: 'include'` (`web/src/lib/api/client.ts`), so the cheap,
  standard fix is origin verification middleware in axum: reject mutating
  requests whose `Origin` (or `Sec-Fetch-Site`) header doesn't match the
  configured `base_url`. A custom-header requirement (e.g. `X-Requested-With`,
  which cross-site HTML forms cannot set) is an alternative; a full
  synchronizer-token scheme is likely overkill for a cookie+SPA app. Noted
  2026-08-06 while fixing the logout 405: logout was kept POST-only
  specifically so forced-logout via GET isn't possible, but the rest of the
  API deserves the same consideration.

## Deploy (required for the Android app to sync)

- [ ] **Redeploy the production server** (rebuild the `rust-note:latest` image
  and restart the compose stack on bigboy). The app's device-token auth
  (`/auth/login?client=app`, bearer on `/api/*`, `?token=` on `/ws/notes/*`),
  the `device_tokens` migration, and the `http://tauri.localhost` CORS default
  all shipped 2026-08-06 and only take effect after a redeploy. No compose
  changes needed (`RUSTNOTE_CORS_ORIGINS` is unset, so the new defaults apply
  — if it is ever set explicitly it must include `http://tauri.localhost`).
- [ ] **Check Caddy access logging** for `notes.osmosis.page`: the app's
  collab websocket carries its device token as `?token=` (browser WebSocket
  API can't set headers). The rust_note server logs no request URLs, but if
  Caddy access logs are enabled they will capture raw tokens for
  `/ws/notes/*` — strip query strings from those log entries or accept the
  exposure (tokens are revocable via logout and expire after 90 days idle).

## Mobile app follow-ups

- [ ] **Verify on a physical device** (riskiest assumption first): the OIDC
  redirect chain must end inside the app — Authentik →
  `notes.osmosis.page/auth/callback` → `http://tauri.localhost/#token=…` has
  to be intercepted by Tauri's Android protocol handler with the URL fragment
  preserved. Contingency if it isn't: `tauri-plugin-deep-link` with a custom
  scheme (`rustnote://auth#token=…`). Also verify: IndexedDB/localStorage
  survive app restarts; websocket reconnect after backgrounding; airplane-mode
  edit → reopen → reconnect → CRDT convergence.
- [ ] **Offline note creation** (deferred from v1): queue
  `{title, tempId, ydocUpdate}` in localStorage, flush on the next successful
  `/api/notes` fetch, then join the room and apply the buffered update.
- [ ] **App webview CSP**: `crates/mobile/tauri.conf.json` has `csp: null`;
  hardening follow-up is `connect-src https://notes.osmosis.page
  wss://notes.osmosis.page` so the webview can only talk to the real backend.
- [ ] **Two-way folder import** (folder → app): the notes-folder mirror is
  one-way (app → `.md` files) in v1. Importing external edits back requires
  change detection over SAF (no file watchers), conflict handling against the
  CRDT doc, and deciding authority — deliberately deferred.
- [ ] **Keyboard-inset polish**: `tauri-plugin-edge-to-edge` also injects
  `--keyboard-height` / `--keyboard-visible`; use them to keep the editor's
  focused line above the soft keyboard.
- [ ] **File-like config entries in the app** (requested 2026-08-07): pin
  `settings`, `notes_sync`, and `select folder` as file-looking entries in the
  app's notes list. Server-side reserved namespace of real notes:
  `.config/settings` (settings are ALREADY stored as a per-user frontmatter
  note server-side — surface it) and `.config/sync/<device>` (per-device
  mirror record: folder display name, enabled, last-mirror time; keyed by the
  device's token so a user's multiple devices each get their own). The SAF
  folder *grant* itself cannot be stored server-side (device-local Android
  security object; must be picked once per device) — only the record of it
  roams. `select folder` entry triggers the SAF picker and writes the result
  into the device's sync note.
