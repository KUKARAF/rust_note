// Offline-first metadata caches, all over localStorage.
//
// The actual note *content* lives in per-note IndexedDB databases managed by
// y-indexeddb (see `collabProvider.ts`) — CRDT updates are the source of
// truth and merge cleanly when connectivity returns. What the CRDT does NOT
// cover is everything the UI needs *around* the content: which notes exist,
// their titles/metadata, and who the signed-in user is. Those come from REST
// endpoints that fail hard when offline, so successful responses are cached
// here and read back when a request fails with a network error (ApiError
// status 0).
//
// Everything here is best-effort: localStorage may be unavailable (privacy
// mode etc.) or hold corrupt data, and in both cases the helpers degrade to
// "no cache" rather than throwing — offline support quietly disappears but
// the online app keeps working. All caches are cleared on logout (see the
// root layout) so a shared device doesn't keep note titles or identity
// around.

import type { AuthUser } from './auth';

/** Note metadata as returned by `GET /api/notes` (list shape). */
export interface CachedNoteListItem {
	id: string;
	title: string;
	owner_id: string;
	created_at: string;
	updated_at: string;
}

/** Note metadata as returned by `GET /api/notes/{path}` (adds `version`). */
export interface CachedNoteMeta extends CachedNoteListItem {
	version: string;
}

const SYNCED_NOTES_KEY = 'rustnote_synced_notes';
const NOTES_CACHE_KEY = 'rustnote_notes_cache';
const NOTE_META_PREFIX = 'rustnote_note_meta:';
const CACHED_USER_KEY = 'rustnote_cached_user';

/** Parse a JSON localStorage entry, returning `null` on absence/corruption. */
function readJson(key: string): unknown {
	try {
		const raw = localStorage.getItem(key);
		return raw === null ? null : (JSON.parse(raw) as unknown);
	} catch {
		// localStorage unavailable or corrupt JSON — treat as "no cache"
		return null;
	}
}

function writeJson(key: string, value: unknown): void {
	try {
		localStorage.setItem(key, JSON.stringify(value));
	} catch {
		// best-effort only
	}
}

function removeKey(key: string): void {
	try {
		localStorage.removeItem(key);
	} catch {
		// best-effort only
	}
}

// --- Synced-notes registry ---------------------------------------------------
//
// Records which note paths have fully synced their CRDT room at least once on
// this device, i.e. which notes have a usable y-indexeddb copy. This is what
// lets the editor page decide "openable offline" without probing IndexedDB
// (there's no cheap existence check), and it doubles as the DB-name source for
// `clearAllLocalNotes()` on browsers without `indexedDB.databases()`.

function readSyncedNotes(): string[] {
	const parsed = readJson(SYNCED_NOTES_KEY);
	return Array.isArray(parsed) ? parsed.filter((p): p is string => typeof p === 'string') : [];
}

/** Record that `path` has a local CRDT copy (called when a room first syncs). */
export function markNoteSynced(path: string): void {
	const synced = readSyncedNotes();
	if (synced.includes(path)) return;
	synced.push(path);
	writeJson(SYNCED_NOTES_KEY, synced);
}

/** Whether `path` has a local CRDT copy and can be opened offline. */
export function hasLocalCopy(path: string): boolean {
	return readSyncedNotes().includes(path);
}

export function listSyncedNotes(): string[] {
	return readSyncedNotes();
}

export function clearSyncedRegistry(): void {
	removeKey(SYNCED_NOTES_KEY);
}

// --- Notes-list cache --------------------------------------------------------

export interface NotesListCache {
	/** `Date.now()` at the time the list was fetched, for an "N min ago" hint. */
	at: number;
	notes: CachedNoteListItem[];
}

/** Cache a successful `GET /api/notes` response for offline fallback. */
export function cacheNotesList(notes: CachedNoteListItem[]): void {
	writeJson(NOTES_CACHE_KEY, { at: Date.now(), notes } satisfies NotesListCache);
}

export function readNotesListCache(): NotesListCache | null {
	const parsed = readJson(NOTES_CACHE_KEY);
	if (
		parsed !== null &&
		typeof parsed === 'object' &&
		typeof (parsed as NotesListCache).at === 'number' &&
		Array.isArray((parsed as NotesListCache).notes)
	) {
		return parsed as NotesListCache;
	}
	return null;
}

export function clearNotesListCache(): void {
	removeKey(NOTES_CACHE_KEY);
}

// --- Per-note metadata cache -------------------------------------------------
//
// The editor page needs a note's metadata (title etc.) to render its chrome;
// offline, the y-indexeddb copy only has the content, so the last-seen meta
// from `GET /api/notes/{path}` is cached alongside it.

/** Cache a successful `GET /api/notes/{path}` meta for offline opens. */
export function cacheNoteMeta(path: string, meta: CachedNoteMeta): void {
	writeJson(`${NOTE_META_PREFIX}${path}`, meta);
}

export function readNoteMeta(path: string): CachedNoteMeta | null {
	const parsed = readJson(`${NOTE_META_PREFIX}${path}`);
	if (
		parsed !== null &&
		typeof parsed === 'object' &&
		typeof (parsed as CachedNoteMeta).id === 'string'
	) {
		return parsed as CachedNoteMeta;
	}
	return null;
}

/** Remove every `rustnote_note_meta:*` entry (logout cleanup). */
export function clearAllNoteMeta(): void {
	try {
		// Collect first: removing while iterating shifts localStorage indices.
		const keys: string[] = [];
		for (let i = 0; i < localStorage.length; i++) {
			const key = localStorage.key(i);
			if (key !== null && key.startsWith(NOTE_META_PREFIX)) keys.push(key);
		}
		keys.forEach((key) => localStorage.removeItem(key));
	} catch {
		// best-effort only
	}
}

// --- Cached user -------------------------------------------------------------
//
// Lets the app shell (and the collab caret identity) keep working offline:
// `loadUser()` falls back to this when `GET /auth/me` fails with a network
// error. A real 401 clears it — "logged out" must win over any stale cache.

export function cacheUser(user: AuthUser): void {
	writeJson(CACHED_USER_KEY, user);
}

export function readCachedUser(): AuthUser | null {
	const parsed = readJson(CACHED_USER_KEY);
	if (
		parsed !== null &&
		typeof parsed === 'object' &&
		typeof (parsed as AuthUser).id === 'string'
	) {
		return parsed as AuthUser;
	}
	return null;
}

export function clearCachedUser(): void {
	removeKey(CACHED_USER_KEY);
}
