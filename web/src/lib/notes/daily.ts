// Daily-note helpers: the vault convention (mirroring the user's existing
// vimwiki/Obsidian diary) is one note per day at `diary/YYYY-MM-DD`.

import { goto } from '$app/navigation';
import { resolve } from '$app/paths';
import { apiGet, apiPost, ApiError } from '$lib/api/client';
import { hasLocalCopy } from '$lib/stores/offline';
import { flagLoginRequired } from '$lib/stores/auth';
import { encodeNotePath } from '$lib/notes/path';

/** Matches diary note ids (`diary/2026-08-07`) — drives the track button. */
export const DAILY_NOTE_RE = /^diary\/\d{4}-\d{2}-\d{2}$/;

/**
 * Seed content for a freshly created daily note. Matches the user's recent
 * live diary files (their Obsidian template evolved to a bare `plan: true`).
 * A template-note lookup (Templates/Diary) is a documented follow-up.
 */
const DAILY_TEMPLATE = '---\nplan: true\n---\n';

/**
 * Today's note id using the LOCAL date. Deliberately not
 * `toISOString().slice(0, 10)` — that is UTC and points at the wrong day
 * between local midnight and UTC midnight.
 */
export function todayNoteId(): string {
	const now = new Date();
	const y = now.getFullYear();
	const m = String(now.getMonth() + 1).padStart(2, '0');
	const d = String(now.getDate()).padStart(2, '0');
	return `diary/${y}-${m}-${d}`;
}

/**
 * Open today's daily note, creating it (with the template) if it doesn't
 * exist yet. Returns null on success, or a human-readable error message the
 * caller may surface (e.g. offline with no cached copy).
 */
export async function openTodayNote(): Promise<string | null> {
	const id = todayNoteId();
	const target = resolve(`/notes/${encodeNotePath(id)}`);

	try {
		await apiGet(`/api/notes/${encodeNotePath(id)}`);
		await goto(target);
		return null;
	} catch (err) {
		if (err instanceof ApiError && err.status === 404) {
			try {
				await apiPost('/api/notes', { id_or_title: id, content: DAILY_TEMPLATE });
			} catch (createErr) {
				// 409: someone (another device/tab) created it between our GET
				// and POST — that's fine, it exists now.
				if (!(createErr instanceof ApiError && createErr.status === 409)) {
					return createErr instanceof Error ? createErr.message : "Couldn't create today's note";
				}
			}
			await goto(target);
			return null;
		}
		if (err instanceof ApiError && err.status === 0) {
			// Offline: a previously synced copy opens fine through the editor's
			// offline path; without one there is nothing to show.
			if (hasLocalCopy(id)) {
				await goto(target);
				return null;
			}
			return "You're offline and today's note isn't cached on this device.";
		}
		if (err instanceof ApiError && err.status === 401) {
			flagLoginRequired();
			await goto(resolve('/login'));
			return null;
		}
		return err instanceof Error ? err.message : "Couldn't open today's note";
	}
}
