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
