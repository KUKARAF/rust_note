# rust-note

Self-hosted, git-backed, vimwiki-style notes. A Rust ([axum]) server stores
notes as plain Markdown files in a git repository (every save is a commit)
with SQLite for users/ACLs/sessions; a SvelteKit SPA provides the web UI with
real-time collaborative editing (Yjs/[yrs] CRDTs over WebSocket); a Tauri 2
Android app wraps the same frontend with offline-first local persistence and
foreground sync. Authentication is OIDC against [Authentik].

[axum]: https://github.com/tokio-rs/axum
[yrs]: https://github.com/y-crdt/y-crdt
[Authentik]: https://goauthentik.io/

## Repo layout

| Path | What it is |
| --- | --- |
| `crates/server` | axum backend: REST API, OIDC auth, collab WebSocket, git+SQLite storage |
| `crates/core` | shared platform-independent domain logic (frontmatter, tokens, slugs) |
| `crates/mobile` | Tauri 2 Android app shell (`gen/` is generated in CI, never committed) |
| `web/` | SvelteKit 2 / Svelte 5 SPA (CodeMirror 6 + Yjs editor) |
| `deploy/` | production docker-compose (behind a Caddy reverse proxy) |

## Quickstart (dev)

Dev mode skips OIDC entirely (every request is user `admin`) and only ever
binds to loopback:

```bash
# terminal 1 — backend on :8080
RUSTNOTE_ENV=dev cargo run -p server

# terminal 2 — frontend on :5173 (proxies API calls to :8080)
cd web && npm install && npm run dev
```

To produce the app-flavored frontend bundle (used by the Android build):
`PUBLIC_API_BASE_URL=<server url> PUBLIC_APP_MODE=tauri npm run build`.

## Authentik OIDC setup

1. In Authentik, create an **OAuth2/OpenID Provider** with client type
   **Public** (PKCE only — no client secret).
2. Set the provider's **Authorized Redirect URI** to exactly:

   ```
   {RUSTNOTE_BASE_URL}/auth/callback
   ```

   e.g. for production: `https://notes.osmosis.page/auth/callback`. This must
   match `RUSTNOTE_OIDC_REDIRECT_URI` byte-for-byte (dev default:
   `http://localhost:8080/auth/callback`).
3. Scopes: `openid`, `profile`, `email`.
4. Copy the provider's **issuer URL** into `RUSTNOTE_AUTHENTIK_ISSUER_URL`
   and the **client ID** into `RUSTNOTE_OIDC_CLIENT_ID`. Leave
   `RUSTNOTE_OIDC_CLIENT_SECRET` unset for a Public client.

> **Android app note:** the app does **not** need its own redirect URI
> registered in Authentik. The app opens the same server login
> (`/auth/login?client=app`); after the server's `/auth/callback` completes
> the OIDC exchange, the server hands a device token back to the app via a
> redirect to `http://tauri.localhost/#token=…` — that handoff happens after
> the OIDC flow has already ended and never reaches Authentik.

## Environment variables

All server configuration is via `RUSTNOTE_*` env vars (see
`crates/server/src/config.rs` for authoritative defaults):

| Variable | Meaning (default) |
| --- | --- |
| `RUSTNOTE_BASE_URL` | public URL of the deployment (`http://localhost:8080`; the dev default is refused outside dev mode) |
| `RUSTNOTE_BIND_ADDR` | listen address (`127.0.0.1:8080`) |
| `RUSTNOTE_AUTHENTIK_ISSUER_URL` | OIDC issuer base URL |
| `RUSTNOTE_OIDC_CLIENT_ID` | OIDC client id |
| `RUSTNOTE_OIDC_CLIENT_SECRET` | only for Confidential clients; unset = Public/PKCE |
| `RUSTNOTE_OIDC_REDIRECT_URI` | must equal `{RUSTNOTE_BASE_URL}/auth/callback` and the URI registered in Authentik |
| `RUSTNOTE_COOKIE_SIGNING_KEY` | session-cookie signing key, ≥32 decoded bytes (dev default refused outside dev mode) |
| `RUSTNOTE_COOKIE_SECURE` | `true`/`false`; default heuristic: secure iff base URL is https |
| `RUSTNOTE_NOTES_REPO_PATH` | git-backed notes directory (`./data/notes`) |
| `RUSTNOTE_SQLITE_PATH` | SQLite file (`./data/rust_note.db`) |
| `RUSTNOTE_STATIC_DIR` | built `web/build` to serve as the SPA (unset in dev; Vite serves it) |
| `RUSTNOTE_CORS_ORIGINS` | comma-separated allowlist. **Setting it replaces the defaults entirely** — it must then include `http://tauri.localhost` or the Android app breaks |
| `RUSTNOTE_ENV=dev` / `RUSTNOTE_DEV_MODE=true` | auth bypass for local dev — never in production |

## Deployment

The root `Dockerfile` builds a self-contained image (frontend build → release
server build → slim runtime serving the SPA itself on :8080).
`deploy/docker-compose.yml` runs it behind an external Caddy proxy network
with the notes directory and SQLite volume mounted. After changing server
code, rebuild the image and restart the stack.

Operator note: the Android app's collab WebSocket authenticates with a
`?token=` query parameter (the browser WebSocket API cannot set headers).
The server logs no request URLs, but if your reverse proxy's access logs are
enabled they will capture raw tokens for `/ws/notes/*` — strip query strings
from those log entries or accept the exposure (tokens are revocable via
logout and expire after 90 days idle).

## Android app

Install the debug APK from the rolling [`nightly` pre-release] or a signed
release APK from a `v*` tag release. CI (`.github/workflows/android-*.yml`)
builds the frontend with `PUBLIC_API_BASE_URL=https://notes.osmosis.page`
and `PUBLIC_APP_MODE=tauri`, regenerates the Android Gradle project with
`cargo tauri android init` (so **never customize `crates/mobile/gen/`** — it
does not persist), and builds with `cargo tauri android build --apk`.
Tagged releases are signed with a keystore held in repo secrets; the tag
must match the `version` in `crates/mobile/tauri.conf.json`.

Tauri capabilities live in `crates/mobile/capabilities/` (source-controlled,
outside `gen/`). The `tauri-plugin-android-fs` crate and the npm
`tauri-plugin-android-fs-api` package are version-locked and must always be
bumped **together** to the exact same version.

[`nightly` pre-release]: https://github.com/KUKARAF/rust_note/releases/tag/nightly

## CI

- `ci-required.yml` — blocking: rustfmt, workspace build/test (mobile crate
  excluded — it only compiles in the Android workflows), panic-safety clippy
  gate (`unwrap`/`expect`/`panic`/… are `deny` in production code), frontend
  check + build.
- `ci-advisory.yml` — non-blocking clippy `pedantic`/`nursery` PR comments.
- `android-nightly.yml` / `android-release.yml` — debug APK per `main` push
  touching app-relevant paths; signed APK + GitHub Release per `v*` tag.
