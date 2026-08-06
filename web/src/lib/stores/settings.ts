import { writable } from 'svelte/store';
import { apiGet, apiPut, ApiError } from '$lib/api/client';

// Widen this union (and `THEMES` in the settings page) when a 2nd theme ships.
export type Theme = 'ration';

export interface SettingsState {
	theme: Theme;
	loading: boolean;
}

const STORAGE_KEY = 'rust-note-theme';

function readCachedTheme(): Theme {
	try {
		const cached = localStorage.getItem(STORAGE_KEY);
		if (cached === 'ration') return cached;
	} catch {
		// localStorage unavailable (privacy mode etc.) — fall through to default
	}
	return 'ration';
}

function cacheTheme(theme: Theme): void {
	try {
		localStorage.setItem(STORAGE_KEY, theme);
	} catch {
		// best-effort only
	}
}

// Populated by calling `GET /api/settings` on startup via `loadSettings()`
// (invoked once from the root `+layout.svelte`), and updated via `setTheme()`.
// Seeded from a locally cached theme so the UI doesn't flash a default while
// the network request is in flight.
export const settings = writable<SettingsState>({
	theme: readCachedTheme(),
	loading: true
});

/**
 * Fetches the current settings from the backend and updates the store.
 *
 * A 401 response means "not logged in" — this is a normal, expected state
 * (not an error), so the cached/default theme is kept rather than surfacing
 * an error anywhere.
 */
export async function loadSettings(): Promise<void> {
	settings.update((s) => ({ ...s, loading: true }));

	try {
		const result = await apiGet<{ theme: Theme }>('/api/settings');
		cacheTheme(result.theme);
		settings.set({ theme: result.theme, loading: false });
	} catch (err) {
		if (err instanceof ApiError && err.status === 401) {
			settings.update((s) => ({ ...s, loading: false }));
			return;
		}
		// Network error or unexpected failure: keep the cached/default theme so
		// the UI doesn't get stuck in a loading state, but log for visibility.
		console.error('Failed to load settings', err);
		settings.update((s) => ({ ...s, loading: false }));
	}
}

export async function setTheme(theme: Theme): Promise<void> {
	const result = await apiPut<{ theme: Theme }>('/api/settings', { theme });
	cacheTheme(result.theme);
	settings.update((s) => ({ ...s, theme: result.theme }));
}
