// Shared model for the command palette AND the /notes inline search, so the
// two surfaces stay one implementation. `buildActions` produces the command
// items; `filterItems` merges matching commands + notes into one list.

import { writable } from 'svelte/store';
import { goto } from '$app/navigation';
import { resolve } from '$app/paths';
import { apiPost, API_BASE_URL } from '$lib/api/client';
import { IS_APP } from '$lib/api/deviceToken';
import { encodeNotePath } from '$lib/notes/path';
import { openTodayNote } from '$lib/notes/daily';
import { EMPTY_SCENE } from '$lib/notes/excalidraw';
import { chooseMirrorFolder, mirrorAllSyncedNotes } from '$lib/app/noteMirror';
import { logout } from '$lib/app/logout';
import type { AuthUser } from '$lib/stores/auth';

export interface NoteMeta {
	id: string;
	title: string;
	owner_id: string;
	created_at: string;
	updated_at: string;
}

export interface PaletteItem {
	id: string;
	label: string;
	hint?: string;
	group: 'action' | 'note';
	run: () => void | Promise<void>;
}

/** Notes shown in the modal palette are capped so a huge vault doesn't flood it. */
export const MAX_NOTE_RESULTS = 12;

/**
 * Focus handle for the /notes inline search. The notes page registers its
 * input's focus fn here on mount (and clears it on destroy); the layout's
 * swipe-down gesture calls it when set (i.e. we're on /notes) instead of
 * opening the modal palette — so there's a single search surface per screen.
 */
export const notesSearchFocuser = writable<null | (() => void)>(null);

function login() {
	// Same top-level navigation the login page uses: the OIDC flow must run as
	// page navigations (and `?client=app` routes the callback back into the app
	// with a device token).
	window.location.href = `${API_BASE_URL}/auth/login${IS_APP ? '?client=app' : ''}`;
}

/**
 * Context-aware command list, recomputed from the auth state so it can never
 * offer "Log in" to a logged-in user or vice versa.
 */
export function buildActions(ctx: { user: AuthUser | null }): PaletteItem[] {
	if (ctx.user === null) {
		return [
			{ id: 'login', label: 'Log in', hint: 'open the sign-in flow', group: 'action', run: login }
		];
	}
	const items: PaletteItem[] = [
		{
			id: 'today',
			label: "Today's note",
			hint: 'open or create the daily note',
			group: 'action',
			run: () => openTodayNote().then(() => undefined)
		},
		{
			id: 'todos',
			label: 'Todos',
			hint: 'board of tasks across daily notes',
			group: 'action',
			run: () => goto(resolve('/todo'))
		},
		{
			id: 'new-drawing',
			label: 'New drawing',
			hint: 'create an Excalidraw note',
			group: 'action',
			run: async () => {
				// window.prompt rather than a bespoke dialog: the surface closes
				// before the action runs, so there is no input left to host — and
				// prompt() works fine in the Tauri webview too.
				const name = window.prompt('Name for the new drawing:');
				if (name === null || name.trim() === '') return;
				const meta = await apiPost<{ id: string }>('/api/notes', {
					id_or_title: `${name.trim()}.excalidraw`,
					content: EMPTY_SCENE
				});
				await goto(resolve(`/notes/${encodeNotePath(meta.id)}`));
			}
		},
		{
			id: 'notes',
			label: 'Notes',
			hint: 'open the note list',
			group: 'action',
			run: () => goto(resolve('/notes'))
		},
		{
			id: 'settings',
			label: 'Settings',
			hint: 'theme, AI & app options',
			group: 'action',
			run: () => goto(resolve('/settings'))
		}
	];
	if (IS_APP) {
		items.push(
			{
				id: 'select-folder',
				label: 'Select notes folder',
				hint: 'mirror notes as .md files',
				group: 'action',
				run: () => chooseMirrorFolder().then(() => undefined)
			},
			{
				id: 'rerun-mirror',
				label: 'Re-run notes mirror',
				hint: 'rewrite all mirrored files',
				group: 'action',
				run: () => mirrorAllSyncedNotes().then(() => undefined)
			}
		);
	}
	items.push(
		{
			id: 'reindex',
			label: 'Reindex notes',
			hint: 'import files added outside the app',
			group: 'action',
			run: async () => {
				await apiPost('/api/reindex');
			}
		},
		{
			id: 'logout',
			label: 'Log out',
			hint: 'end this session',
			group: 'action',
			run: logout
		}
	);
	return items;
}

/**
 * Merge matching commands + notes into one list: matching actions first, then
 * matching notes. Empty query shows everything. `noteLimit` caps the notes
 * (the modal palette passes `MAX_NOTE_RESULTS`; the /notes page passes
 * `Infinity` to show the whole vault).
 */
export function filterItems(
	query: string,
	actions: PaletteItem[],
	notes: NoteMeta[],
	noteLimit = MAX_NOTE_RESULTS
): PaletteItem[] {
	const q = query.trim().toLowerCase();
	const matchedActions =
		q === '' ? actions : actions.filter((a) => a.label.toLowerCase().includes(q));
	const matchedNotes = (
		q === ''
			? notes
			: notes.filter((n) => n.title.toLowerCase().includes(q) || n.id.toLowerCase().includes(q))
	)
		.slice(0, noteLimit)
		.map((n): PaletteItem => ({
			id: `note:${n.id}`,
			label: n.title,
			hint: n.id === n.title ? undefined : n.id,
			group: 'note',
			run: () => goto(resolve(`/notes/${encodeNotePath(n.id)}`))
		}));
	return [...matchedActions, ...matchedNotes];
}
