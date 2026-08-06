// Minimal fetch wrapper for talking to the rust_note backend.
//
// This is a real, functional module (unlike the other lib/ placeholders):
// later work (auth, notes CRUD, sharing, collab signaling) is expected to
// build directly on top of `apiGet`/`apiPost`/`apiPut`/`apiDelete`.
//
// The base URL can be overridden at build time via the `PUBLIC_API_BASE_URL`
// env var (see Vite's `import.meta.env`) - set in `web/.env.development` so
// `npm run dev` keeps talking to the separately-running backend on :8080.
// When unset (the production build: the backend serves these static assets
// itself, same origin, no separate dev server), it falls back to
// `window.location.origin` so the built bundle isn't hardcoded to any one
// domain - it just calls whatever origin it was loaded from. This code only
// ever runs in the browser (this app is `ssr=false`), so `window` is always
// available here. (A future Settings page may override this further at
// runtime via localStorage, e.g. for the Tauri build where the backend host
// isn't known until first run.)
export const API_BASE_URL: string =
	(import.meta.env.PUBLIC_API_BASE_URL as string | undefined) ?? window.location.origin;

// Device token for the Tauri app build (bearer auth instead of cookies) —
// `getDeviceToken()` returns null on the website build, so nothing changes there.
import { getDeviceToken } from './deviceToken';

/** Thrown by the request helpers below on any non-2xx response or network failure. */
export class ApiError extends Error {
	/** HTTP status code, or 0 if the request never reached the server (network error). */
	readonly status: number;
	/** Parsed JSON error body, if the response had one and was JSON. */
	readonly body: unknown;

	constructor(message: string, status: number, body?: unknown) {
		super(message);
		this.name = 'ApiError';
		this.status = status;
		this.body = body;
	}
}

export interface RequestOptions {
	/** Extra headers to merge into the request. */
	headers?: Record<string, string>;
	/** AbortSignal for cancellation. */
	signal?: AbortSignal;
}

async function request<TResponse>(
	method: 'GET' | 'POST' | 'PUT' | 'PATCH' | 'DELETE',
	path: string,
	body?: unknown,
	options: RequestOptions = {}
): Promise<TResponse> {
	const url = path.startsWith('http') ? path : `${API_BASE_URL}${path}`;
	const deviceToken = getDeviceToken();

	let response: Response;
	try {
		response = await fetch(url, {
			method,
			// Session auth is cookie-based (set by the backend after OIDC login),
			// so credentials must be included on every request, including
			// cross-origin ones (e.g. Tauri webview talking to a remote host).
			credentials: 'include',
			headers: {
				...(body !== undefined ? { 'Content-Type': 'application/json' } : {}),
				Accept: 'application/json',
				// App build: authenticate with the stored device token (cookies
				// don't reliably cross the tauri.localhost <-> backend origin
				// split). Spread before options.headers so callers can override.
				...(deviceToken !== null ? { Authorization: `Bearer ${deviceToken}` } : {}),
				...options.headers
			},
			body: body !== undefined ? JSON.stringify(body) : undefined,
			signal: options.signal
		});
	} catch (cause) {
		throw new ApiError(
			`Network error while requesting ${method} ${url}`,
			0,
			cause instanceof Error ? cause.message : cause
		);
	}

	const contentType = response.headers.get('content-type') ?? '';
	const isJson = contentType.includes('application/json');
	const payload = isJson ? await response.json().catch(() => undefined) : undefined;

	if (!response.ok) {
		const message =
			(isJson && payload && typeof payload === 'object' && 'message' in payload
				? String((payload as Record<string, unknown>).message)
				: undefined) ?? `Request failed with status ${response.status}`;
		throw new ApiError(message, response.status, payload);
	}

	return payload as TResponse;
}

export function apiGet<TResponse>(path: string, options?: RequestOptions): Promise<TResponse> {
	return request<TResponse>('GET', path, undefined, options);
}

export function apiPost<TResponse>(
	path: string,
	body?: unknown,
	options?: RequestOptions
): Promise<TResponse> {
	return request<TResponse>('POST', path, body, options);
}

export function apiPut<TResponse>(
	path: string,
	body?: unknown,
	options?: RequestOptions
): Promise<TResponse> {
	return request<TResponse>('PUT', path, body, options);
}

export function apiPatch<TResponse>(
	path: string,
	body?: unknown,
	options?: RequestOptions
): Promise<TResponse> {
	return request<TResponse>('PATCH', path, body, options);
}

export function apiDelete<TResponse>(path: string, options?: RequestOptions): Promise<TResponse> {
	return request<TResponse>('DELETE', path, undefined, options);
}
