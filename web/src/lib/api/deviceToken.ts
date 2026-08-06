// Device-token auth for the Tauri app build.
//
// The website relies on the backend's session cookie, but the Android app's
// webview loads the UI from `http://tauri.localhost` while the backend lives
// on a remote origin — third-party cookie rules make cookie auth unreliable
// there. Instead, the app build runs the OIDC flow with `?client=app`, and the
// backend finishes it by redirecting to `http://tauri.localhost/#token=<raw>`
// with a long-lived *device token*. The root layout captures that token from
// the URL fragment, stores it here, and from then on:
//
//   - every REST request carries it as `Authorization: Bearer <token>`
//     (see `client.ts`), and
//   - the collab websocket passes it as a `?token=` query param
//     (see `collabProvider.ts`), since websockets can't set headers.
//
// Logout revokes the token server-side (`POST /auth/logout`) and clears it
// here. On the website build none of this is active: `IS_APP` is false and no
// token is ever stored, so the Authorization header is simply never added.

/**
 * True when this bundle was built for the Tauri app (`PUBLIC_APP_MODE=tauri`
 * set at build time, e.g. in the Android workflows). The website build leaves
 * it unset.
 */
export const IS_APP: boolean = (import.meta.env.PUBLIC_APP_MODE as string | undefined) === 'tauri';

const STORAGE_KEY = 'rustnote_device_token';

/** The stored device token, or `null` when absent (website build / logged out). */
export function getDeviceToken(): string | null {
	try {
		return localStorage.getItem(STORAGE_KEY);
	} catch {
		// localStorage unavailable (privacy mode etc.) — behave as logged out
		return null;
	}
}

export function setDeviceToken(token: string): void {
	try {
		localStorage.setItem(STORAGE_KEY, token);
	} catch {
		// best-effort only
	}
}

export function clearDeviceToken(): void {
	try {
		localStorage.removeItem(STORAGE_KEY);
	} catch {
		// best-effort only
	}
}
