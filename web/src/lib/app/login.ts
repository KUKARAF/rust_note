// Start the OIDC login. On the app build, open the OS browser (a Chrome Custom
// Tab) so OAuth runs OUTSIDE the app's webview; the backend then redirects back
// via the `dev.rustnote.app://auth` deep link (see $lib/app/deepLinkAuth and
// crates/server/src/auth/oidc.rs). On the web build, a normal same-tab
// navigation. The `@tauri-apps/plugin-opener` import is dynamic so the web
// build never loads Tauri APIs.
import { API_BASE_URL } from '$lib/api/client';
import { IS_APP } from '$lib/api/deviceToken';

export async function startLogin(): Promise<void> {
	const url = `${API_BASE_URL}/auth/login${IS_APP ? '?client=app' : ''}`;
	if (IS_APP) {
		const { openUrl } = await import('@tauri-apps/plugin-opener');
		await openUrl(url);
	} else {
		window.location.href = url;
	}
}
