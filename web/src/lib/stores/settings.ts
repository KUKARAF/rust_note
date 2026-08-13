import { writable } from 'svelte/store';
import { apiGet, apiPut, ApiError } from '$lib/api/client';

// Widen this union (and `THEMES` in the settings page) when a 2nd theme ships.
export type Theme = 'ration';

export interface SettingsState {
	theme: Theme;
	/** OpenRouter model id used for natural-language todo queries. */
	openrouterModel: string;
	/** Whether an OpenRouter API key is stored server-side (never the key itself). */
	hasOpenrouterKey: boolean;
	loading: boolean;
}

/** Server response shape for `GET`/`PUT /api/settings`. */
interface SettingsResponse {
	theme: Theme;
	openrouter_model: string;
	has_openrouter_key: boolean;
}

const STORAGE_KEY = 'rust-note-theme';
const DEFAULT_MODEL = 'minimax/minimax-m3';

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
// (invoked once from the root `+layout.svelte`), and updated via the setters.
// Seeded from a locally cached theme so the UI doesn't flash a default while
// the network request is in flight. The OpenRouter key is NEVER cached.
export const settings = writable<SettingsState>({
	theme: readCachedTheme(),
	openrouterModel: DEFAULT_MODEL,
	hasOpenrouterKey: false,
	loading: true
});

/**
 * Fetches the current settings from the backend and updates the store.
 *
 * A 401 response means "not logged in" — this is a normal, expected state
 * (not an error), so the cached/default values are kept rather than surfacing
 * an error anywhere.
 */
export async function loadSettings(): Promise<void> {
	settings.update((s) => ({ ...s, loading: true }));

	try {
		const result = await apiGet<SettingsResponse>('/api/settings');
		cacheTheme(result.theme);
		settings.set({
			theme: result.theme,
			openrouterModel: result.openrouter_model || DEFAULT_MODEL,
			hasOpenrouterKey: result.has_openrouter_key,
			loading: false
		});
	} catch (err) {
		if (err instanceof ApiError && err.status === 401) {
			settings.update((s) => ({ ...s, loading: false }));
			return;
		}
		// Network error or unexpected failure: keep the cached/default values so
		// the UI doesn't get stuck in a loading state, but log for visibility.
		console.error('Failed to load settings', err);
		settings.update((s) => ({ ...s, loading: false }));
	}
}

export async function setTheme(theme: Theme): Promise<void> {
	const result = await apiPut<SettingsResponse>('/api/settings', { theme });
	cacheTheme(result.theme);
	settings.update((s) => ({ ...s, theme: result.theme }));
}

export async function setOpenrouterModel(model: string): Promise<void> {
	const result = await apiPut<SettingsResponse>('/api/settings', { openrouter_model: model });
	settings.update((s) => ({ ...s, openrouterModel: result.openrouter_model }));
}

/** Save (or, with an empty string, clear) the OpenRouter API key. */
export async function setOpenrouterKey(key: string): Promise<void> {
	const result = await apiPut<SettingsResponse>('/api/settings', { openrouter_api_key: key });
	settings.update((s) => ({ ...s, hasOpenrouterKey: result.has_openrouter_key }));
}
