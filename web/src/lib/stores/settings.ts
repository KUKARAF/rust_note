import { writable } from 'svelte/store';
import { apiGet, apiPut, ApiError } from '$lib/api/client';

// Widen this union (and `THEMES` in the settings page) when a 2nd theme ships.
export type Theme = 'ration';

/** `sex` — which body-fat chart applies. */
export type BodySex = 'male' | 'female';

export interface SettingsState {
	theme: Theme;
	/** OpenRouter model id used for natural-language todo queries. */
	openrouterModel: string;
	/** OpenRouter model id used for `#AI!` in-note inline edits. */
	aiCommandModel: string;
	/** Whether an OpenRouter API key is stored server-side (never the key itself). */
	hasOpenrouterKey: boolean;
	/** Base URL of the LiteLLM (OpenAI-compatible) proxy used for AI requests. */
	aiEndpoint: string;
	/** Body-fat profile: picks the men/women chart. Empty when not set. */
	sex: BodySex | '';
	/** Birth date `YYYY-MM-DD`, or empty when not set. */
	birthDate: string;
	loading: boolean;
}

/** Server response shape for `GET`/`PUT /api/settings`. */
interface SettingsResponse {
	theme: Theme;
	openrouter_model: string;
	ai_command_model: string;
	has_openrouter_key: boolean;
	ai_endpoint: string;
	sex: BodySex | '';
	birth_date: string;
}

const STORAGE_KEY = 'rust-note-theme';
const DEFAULT_MODEL = 'minimax/minimax-m3';
/** Default for `ai_command_model` — benchmarked clean + fast for `#AI!` edits. */
export const DEFAULT_AI_COMMAND_MODEL = 'openrouter/google/gemini-3.1-flash-lite';
export const DEFAULT_AI_ENDPOINT = 'https://litellm.osmosis.page/v1';

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
	aiCommandModel: DEFAULT_AI_COMMAND_MODEL,
	hasOpenrouterKey: false,
	aiEndpoint: DEFAULT_AI_ENDPOINT,
	sex: '',
	birthDate: '',
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
			aiCommandModel: result.ai_command_model || DEFAULT_AI_COMMAND_MODEL,
			hasOpenrouterKey: result.has_openrouter_key,
			aiEndpoint: result.ai_endpoint || DEFAULT_AI_ENDPOINT,
			sex: result.sex ?? '',
			birthDate: result.birth_date ?? '',
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

export async function setAiCommandModel(model: string): Promise<void> {
	const result = await apiPut<SettingsResponse>('/api/settings', { ai_command_model: model });
	settings.update((s) => ({ ...s, aiCommandModel: result.ai_command_model }));
}

/** Save (or, with an empty string, clear) the OpenRouter API key. */
export async function setOpenrouterKey(key: string): Promise<void> {
	const result = await apiPut<SettingsResponse>('/api/settings', { openrouter_api_key: key });
	settings.update((s) => ({ ...s, hasOpenrouterKey: result.has_openrouter_key }));
}

export async function setAiEndpoint(endpoint: string): Promise<void> {
	const result = await apiPut<SettingsResponse>('/api/settings', { ai_endpoint: endpoint });
	settings.update((s) => ({ ...s, aiEndpoint: result.ai_endpoint }));
}

/** Result shape returned by `GET /api/ai/models`. */
export interface AiModelsResult {
	models: string[];
	error: string | null;
}

/**
 * Lists the models available on the configured LiteLLM proxy. Never throws on
 * upstream failure — the backend always answers 200 with `error` set instead
 * (e.g. "set an API key first"), so callers only need to branch on `error`.
 */
export function fetchAiModels(): Promise<AiModelsResult> {
	return apiGet<AiModelsResult>('/api/ai/models');
}

// --- body-fat profile ------------------------------------------------------

/** Save the body-fat profile (either field; `''` clears it). */
export async function setBodyProfile(profile: {
	sex?: BodySex | '';
	birthDate?: string;
}): Promise<void> {
	const result = await apiPut<SettingsResponse>('/api/settings', {
		sex: profile.sex,
		birth_date: profile.birthDate
	});
	settings.update((s) => ({ ...s, sex: result.sex ?? '', birthDate: result.birth_date ?? '' }));
}
