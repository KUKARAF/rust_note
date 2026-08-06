# Security response headers (set at the reverse proxy / static host)

This app is built with `adapter-static` (a pure client SPA served as plain
files). It has no server runtime of its own, so response headers must be set by
whatever serves `build/` — an nginx/Caddy reverse proxy, a CDN, an object-store
static host, etc. The only security measure baked into the app itself is the
`<meta name="referrer">` tag in `src/app.html`; everything below is deferred to
the host because it either only works as a real HTTP header, or depends on the
deployment's real API origin.

## Headers to set

| Header | Value | Notes |
| --- | --- | --- |
| `Content-Security-Policy` | see policy below | Set as a real header so it can be tuned / made report-only per environment. |
| `X-Content-Type-Options` | `nosniff` | Only effective as a real header — a `<meta http-equiv>` version is ignored by browsers. |
| `X-Frame-Options` | `DENY` | Clickjacking protection. `frame-ancestors` in the CSP is the modern equivalent; keep both for older browsers. |
| `Referrer-Policy` | `strict-origin-when-cross-origin` | Also set via `<meta name="referrer">` in the app as a fallback; the header is authoritative. |
| `Strict-Transport-Security` | `max-age=63072000; includeSubDomains; preload` | HTTPS deployments only. Do not send over plain HTTP / LAN. |

## Content-Security-Policy

Recommended baseline. **You must extend `connect-src`** with the real backend
HTTP and WebSocket origins for the environment (the values below only cover
local dev, where `PUBLIC_API_BASE_URL` defaults to `http://localhost:8080`).

```
default-src 'self';
script-src 'self' 'unsafe-inline';
style-src 'self' 'unsafe-inline';
font-src 'self';
img-src 'self' data:;
connect-src 'self' http://localhost:8080 http://127.0.0.1:8080 ws://localhost:8080 ws://127.0.0.1:8080;
object-src 'none';
base-uri 'self';
frame-ancestors 'none'
```

### Why these values

- **`script-src 'self' 'unsafe-inline'`** — the SvelteKit static build emits a
  small inline bootstrap `<script>` (no nonce) in `200.html` / `index.html`. A
  strict `script-src 'self'` would prevent the app from booting. If your host
  can inject a per-response nonce, prefer `script-src 'self' 'nonce-...'` and
  drop `'unsafe-inline'`.
- **`style-src 'self' 'unsafe-inline'`** — Svelte and CodeMirror inject inline
  `<style>` blocks and inline `style` attributes at runtime. `'unsafe-inline'`
  here is a known, accepted limitation for CodeMirror/Svelte SPAs; without it the
  UI styling breaks.
- **`connect-src`** — the frontend calls the backend API on a **different
  origin** (default `http://localhost:8080`) and will open **WebSocket**
  connections (`ws://` / `wss://`) for realtime collaboration. Both the HTTP and
  WS backend origins must be listed or the app breaks. Production must add its
  real API/WS origin(s), e.g. `https://api.example.com wss://api.example.com`.
- **`font-src 'self'`** — fonts are self-hosted under `/fonts/` (see
  `src/lib/design/tokens.css`); no external font origin is needed.
- **`img-src 'self' data:`** — allows bundled images and `data:` URIs.
- **`object-src 'none'`, `base-uri 'self'`, `frame-ancestors 'none'`** — standard
  hardening. `frame-ancestors` only takes effect as a real HTTP header.

### Rollout tip

Deploy first as `Content-Security-Policy-Report-Only` (real header only; not
available via `<meta>`), watch for violation reports, then switch to the
enforcing `Content-Security-Policy` header once clean.
