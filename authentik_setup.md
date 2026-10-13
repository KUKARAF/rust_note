# Authentik setup

rust_note authenticates the web/Android login (`/auth/login`, session
cookies, device tokens) against [Authentik] over OIDC. This is the **only**
thing in rust_note that needs any Authentik-side configuration.

[Authentik]: https://goauthentik.io/

> **MCP clients (Claude Code, LiteLLM, claude.ai, ...) need none of this.**
> `/mcp` is its own, self-contained OAuth Authorization Server inside
> rust_note (see `crates/server/src/mcp/oauth`) specifically so that
> connecting a new MCP client never requires touching Authentik at all — no
> provider to create, no scopes to add, no token-validity setting to tune.
> If a future change to this doc ever reintroduces an Authentik-side step
> for MCP, it belongs here, not buried in a general README.

## One-time provider setup

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

## Related env vars

See the main environment-variable table in `README.md` for `RUSTNOTE_BASE_URL`,
`RUSTNOTE_AUTHENTIK_ISSUER_URL`, `RUSTNOTE_OIDC_CLIENT_ID`,
`RUSTNOTE_OIDC_CLIENT_SECRET`, and `RUSTNOTE_OIDC_REDIRECT_URI`.
