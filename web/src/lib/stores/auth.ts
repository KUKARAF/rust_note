import { writable } from 'svelte/store';
import { apiGet, ApiError } from '$lib/api/client';

export interface AuthUser {
	id: string;
	email: string | null;
	display_name: string | null;
}

export interface AuthState {
	user: AuthUser | null;
	loading: boolean;
}

// Populated by calling `GET /auth/me` on startup via `loadUser()` (invoked
// once from the root `+layout.svelte`), and updated on login/logout.
export const auth = writable<AuthState>({
	user: null,
	loading: true
});

/**
 * Fetches the current session user from the backend and updates the store.
 *
 * A 401 response means "not logged in" — this is a normal, expected state
 * (not an error), so `user` is simply set to `null` rather than surfacing an
 * error anywhere.
 */
export async function loadUser(): Promise<void> {
	auth.update((state) => ({ ...state, loading: true }));

	try {
		const user = await apiGet<AuthUser>('/auth/me');
		auth.set({ user, loading: false });
	} catch (err) {
		if (err instanceof ApiError && err.status === 401) {
			auth.set({ user: null, loading: false });
			return;
		}
		// Network error or unexpected failure: treat as "not logged in" for now
		// so the UI doesn't get stuck in a loading state, but log for visibility.
		console.error('Failed to load current user', err);
		auth.set({ user: null, loading: false });
	}
}
