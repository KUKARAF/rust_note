import { writable } from 'svelte/store';
import { apiGet, apiPut, ApiError } from '$lib/api/client';

// Widen this union (and `THEMES` in the settings page) when a 2nd theme ships.
export type Theme = 'ration';

/** `notify_scope` — which side of the pipeline ball counts as overdue for the digest. */
export type NotifyScope = 'ours' | 'theirs' | 'both';

/** `notify_priority` — priority-notify's own priority levels. */
export type NotifyPriority = 'low' | 'medium' | 'high' | 'critical';

export interface SettingsState {
	theme: Theme;
	/** OpenRouter model id used for natural-language todo queries. */
	openrouterModel: string;
	/** Whether an OpenRouter API key is stored server-side (never the key itself). */
	hasOpenrouterKey: boolean;
	/** Base URL of the LiteLLM (OpenAI-compatible) proxy used for AI requests. */
	aiEndpoint: string;
	/** Whether the priority-notify daily digest is enabled. */
	notifyEnabled: boolean;
	/** Base URL of the priority-notify server (path is appended server-side). */
	notifyEndpoint: string;
	/** Priority level attached to each digest push. */
	notifyPriority: NotifyPriority;
	/** Which overdue pipeline items count towards the digest. */
	notifyScope: NotifyScope;
	/** RRULE string controlling when the digest fires. */
	notifySchedule: string;
	/** Whether a priority-notify API token is stored server-side (never the token itself). */
	hasNotifyToken: boolean;
	loading: boolean;
}

/** Server response shape for `GET`/`PUT /api/settings`. */
interface SettingsResponse {
	theme: Theme;
	openrouter_model: string;
	has_openrouter_key: boolean;
	ai_endpoint: string;
	notify_enabled: boolean;
	notify_endpoint: string;
	notify_priority: NotifyPriority;
	notify_scope: NotifyScope;
	notify_schedule: string;
	has_notify_token: boolean;
}

const STORAGE_KEY = 'rust-note-theme';
const DEFAULT_MODEL = 'minimax/minimax-m3';
export const DEFAULT_AI_ENDPOINT = 'https://litellm.osmosis.page/v1';
export const DEFAULT_NOTIFY_ENDPOINT = 'https://notifications.osmosis.page';
export const DEFAULT_NOTIFY_SCHEDULE = 'FREQ=DAILY;BYHOUR=8;BYMINUTE=0';

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
	aiEndpoint: DEFAULT_AI_ENDPOINT,
	notifyEnabled: false,
	notifyEndpoint: DEFAULT_NOTIFY_ENDPOINT,
	notifyPriority: 'high',
	notifyScope: 'both',
	notifySchedule: DEFAULT_NOTIFY_SCHEDULE,
	hasNotifyToken: false,
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
			aiEndpoint: result.ai_endpoint || DEFAULT_AI_ENDPOINT,
			notifyEnabled: result.notify_enabled,
			notifyEndpoint: result.notify_endpoint || DEFAULT_NOTIFY_ENDPOINT,
			notifyPriority: result.notify_priority || 'high',
			notifyScope: result.notify_scope || 'both',
			notifySchedule: result.notify_schedule || DEFAULT_NOTIFY_SCHEDULE,
			hasNotifyToken: result.has_notify_token,
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

// --- priority-notify digest ------------------------------------------------

export async function setNotifyEnabled(enabled: boolean): Promise<void> {
	const result = await apiPut<SettingsResponse>('/api/settings', { notify_enabled: enabled });
	settings.update((s) => ({ ...s, notifyEnabled: result.notify_enabled }));
}

export async function setNotifyScope(scope: NotifyScope): Promise<void> {
	const result = await apiPut<SettingsResponse>('/api/settings', { notify_scope: scope });
	settings.update((s) => ({ ...s, notifyScope: result.notify_scope }));
}

export async function setNotifyPriority(priority: NotifyPriority): Promise<void> {
	const result = await apiPut<SettingsResponse>('/api/settings', { notify_priority: priority });
	settings.update((s) => ({ ...s, notifyPriority: result.notify_priority }));
}

export async function setNotifySchedule(rrule: string): Promise<void> {
	const result = await apiPut<SettingsResponse>('/api/settings', { notify_schedule: rrule });
	settings.update((s) => ({ ...s, notifySchedule: result.notify_schedule }));
}

export async function setNotifyEndpoint(endpoint: string): Promise<void> {
	const result = await apiPut<SettingsResponse>('/api/settings', { notify_endpoint: endpoint });
	settings.update((s) => ({ ...s, notifyEndpoint: result.notify_endpoint }));
}

/** Save (or, with an empty string, clear) the priority-notify API token. */
export async function setNotifyToken(token: string): Promise<void> {
	const result = await apiPut<SettingsResponse>('/api/settings', { notify_token: token });
	settings.update((s) => ({ ...s, hasNotifyToken: result.has_notify_token }));
}

/** Result shape returned by `POST /api/settings/notify-test`. */
export interface NotifyTestResult {
	sent: boolean;
	count: number;
	title: string;
	message: string;
}
