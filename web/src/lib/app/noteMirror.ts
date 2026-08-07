// One-way mirror of synced notes into a user-chosen Android folder (SAF).
//
// The Tauri Android app lets the user authorize a device folder via the system
// SAF picker; every note that has a local CRDT copy is then written into that
// folder as a plain `.md` file (and re-written ~2s after edits, see
// `scheduleMirror`). This is strictly app -> folder: nothing is ever read back
// from the folder, so other apps (file managers, Obsidian, sync tools) get a
// readable export without any risk of them corrupting the CRDT state.
//
// The android-fs plugin module is ONLY ever loaded via dynamic `import()`
// behind an `IS_APP` check (plus a runtime `isAndroid()` check) — the website
// build must never execute it, and keeping the import dynamic keeps the plugin
// out of the entry bundle entirely. Every exported async function therefore
// starts with the cheap `IS_APP` gate and degrades to a no-op elsewhere.
//
// localStorage layout (same best-effort try/catch style as
// `$lib/stores/offline`):
//   - `rustnote_mirror_dir`:   `{ uri: FsUri, name: string }` — the authorized
//     folder's SAF uri + display name (for the settings status line).
//   - `rustnote_mirror_files`: `Record<notePath, FsUri>` — the SAF uri of the
//     file created for each note. This map is essential, not an optimization:
//     SAF's `createNewFile` UNIQUIFIES names (creating `foo.md` twice yields
//     `foo (1).md`), so each note's file must be created exactly once and its
//     uri remembered; all later writes go through `writeTextFile(uri, ...)`
//     which overwrites in place.
//   - `rustnote_mirror_dismissed`: `'1'` — the user tapped "Not now" on the
//     startup prompt; don't nag again (settings still offers the feature).

import type { FsUri } from 'tauri-plugin-android-fs-api';
import * as Y from 'yjs';
import { IndexeddbPersistence } from 'y-indexeddb';
import { IS_APP } from '$lib/api/deviceToken';
import { listSyncedNotes } from '$lib/stores/offline';
import { LOCAL_NOTE_DB_PREFIX } from '$lib/editor/collabProvider';

const MIRROR_DIR_KEY = 'rustnote_mirror_dir';
const MIRROR_FILES_KEY = 'rustnote_mirror_files';
const MIRROR_DISMISSED_KEY = 'rustnote_mirror_dismissed';

/** How long after the last edit a note is (re)written to the folder. */
const MIRROR_DEBOUNCE_MS = 2000;

export interface MirrorState {
	/**
	 * `unset`: no folder chosen; `authorized`: folder chosen and the persisted
	 * SAF grant still holds; `revoked`: a folder is stored but Android no
	 * longer grants access (user revoked it, or the folder was deleted) — the
	 * user has to pick again.
	 */
	status: 'unset' | 'authorized' | 'revoked';
	folderName?: string;
}

type AndroidFs = typeof import('tauri-plugin-android-fs-api');

// Lazy singleton for the plugin module. Resolves to `null` on the website
// build, on non-Android platforms, and if the import itself fails — callers
// treat all three the same way (mirroring unavailable, no-op).
let fsPromise: Promise<AndroidFs | null> | null = null;

function loadFs(): Promise<AndroidFs | null> {
	if (!IS_APP) return Promise.resolve(null);
	if (fsPromise === null) {
		fsPromise = import('tauri-plugin-android-fs-api').then(
			(mod) => (mod.isAndroid() ? mod : null),
			(err) => {
				console.warn('note mirror: android-fs plugin unavailable', err);
				return null;
			}
		);
	}
	return fsPromise;
}

// --- localStorage helpers (best-effort, mirroring $lib/stores/offline) -------

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

interface MirrorDir {
	uri: FsUri;
	name: string;
}

/** Structural check for a stored `FsUri` (can't use the plugin's `isFsUri` guard here — it lives in the dynamically imported module). */
function looksLikeFsUri(value: unknown): value is FsUri {
	if (value === null || typeof value !== 'object') return false;
	const uri = value as FsUri;
	return (
		typeof uri.uri === 'string' &&
		(uri.documentTopTreeUri === null || typeof uri.documentTopTreeUri === 'string')
	);
}

function readMirrorDir(): MirrorDir | null {
	const parsed = readJson(MIRROR_DIR_KEY);
	if (
		parsed !== null &&
		typeof parsed === 'object' &&
		typeof (parsed as MirrorDir).name === 'string' &&
		looksLikeFsUri((parsed as MirrorDir).uri)
	) {
		return parsed as MirrorDir;
	}
	return null;
}

function readMirrorFiles(): Record<string, FsUri> {
	const parsed = readJson(MIRROR_FILES_KEY);
	if (parsed === null || typeof parsed !== 'object' || Array.isArray(parsed)) return {};
	const files: Record<string, FsUri> = {};
	for (const [path, uri] of Object.entries(parsed as Record<string, unknown>)) {
		if (looksLikeFsUri(uri)) files[path] = uri;
	}
	return files;
}

// --- State / authorization ---------------------------------------------------

/**
 * Current mirror configuration, validated against Android's actual persisted
 * uri-permission table — the stored dir alone proves nothing, since the user
 * can revoke the grant (or delete the folder) at any time behind our back.
 */
export async function getMirrorState(): Promise<MirrorState> {
	const fs = await loadFs();
	if (fs === null) return { status: 'unset' };
	const dir = readMirrorDir();
	if (dir === null) return { status: 'unset' };
	try {
		const granted = await fs.checkPersistedPickerUriPermission(dir.uri, 'ReadAndWrite');
		return granted
			? { status: 'authorized', folderName: dir.name }
			: { status: 'revoked', folderName: dir.name };
	} catch {
		return { status: 'revoked', folderName: dir.name };
	}
}

/**
 * Open the SAF directory picker, persist the grant, remember the folder, and
 * mirror every synced note into it. Returns `false` when the user cancels the
 * picker (or mirroring is unavailable), `true` once the initial mirror pass
 * finished. Replacing a previously chosen folder releases the old grant —
 * Android caps persisted uri permissions per app, so stale grants shouldn't
 * accumulate — and resets the note -> file uri map (the old uris point into
 * the old tree).
 */
export async function chooseMirrorFolder(
	onProgress?: (done: number, total: number) => void
): Promise<boolean> {
	const fs = await loadFs();
	if (fs === null) return false;

	const picked = await fs.showOpenDirPicker();
	if (picked === null) return false; // user backed out of the picker

	// Persist the new grant FIRST — only then is it safe to drop the old one.
	await fs.persistPickerUriPermission(picked);

	const previous = readMirrorDir();
	if (previous !== null && previous.uri.uri !== picked.uri) {
		try {
			await fs.releasePersistedPickerUriPermission(previous.uri);
		} catch {
			// best-effort: a leaked grant is harmless, just slightly wasteful
		}
	}

	let name = 'folder';
	try {
		name = await fs.getName(picked);
	} catch {
		// display-only; fall back to a generic label rather than failing
	}

	writeJson(MIRROR_DIR_KEY, { uri: picked, name } satisfies MirrorDir);
	// New tree, new files: every note must go through createNewFile again.
	removeKey(MIRROR_FILES_KEY);
	// Choosing a folder is the opposite of dismissing the prompt.
	removeKey(MIRROR_DISMISSED_KEY);
	revokedWarned = false;

	await mirrorAllSyncedNotes(onProgress);
	return true;
}

// --- Writing -----------------------------------------------------------------

/**
 * Map a note path to a safe relative path inside the mirror folder. Notes keep
 * their folder structure (`diary/2026-06-24` -> `diary/2026-06-24.md`); each
 * segment is scrubbed of characters SAF/FAT-ish filesystems reject and of
 * leading dots (hidden files would silently vanish from most file managers).
 */
export function sanitizeRelPath(notePath: string): string {
	const segments = notePath.split('/').map((segment) => {
		let clean = segment
			.replace(/[\\:*?"<>|]/g, '_')
			.replace(/^\.+/, (dots) => '_'.repeat(dots.length))
			.trim();
		if (clean === '') clean = '_';
		return clean;
	});
	const joined = segments.join('/');
	// Drawing notes are bare Excalidraw scene JSON — mirroring them as
	// `foo.excalidraw.md` would mislabel them for other apps (file managers,
	// Obsidian's excalidraw plugin expect the bare extension). Everything
	// else keeps the historical `.md` suffix.
	return joined.endsWith('.excalidraw') ? joined : `${joined}.md`;
}

// The revoked-grant failure mode hits EVERY debounced write once the user
// pulls the folder's permission; warn a single time instead of spamming the
// console on each keystroke's trailing write.
let revokedWarned = false;

/**
 * Core write shared by `writeMirrorFile` and `mirrorAllSyncedNotes`; returns
 * whether the note actually landed in the folder (the public wrapper hides
 * the boolean, but the bulk pass needs it for its written/failed counts).
 */
async function writeMirrorFileInner(
	fs: AndroidFs,
	dir: MirrorDir,
	notePath: string,
	content: string
): Promise<boolean> {
	const files = readMirrorFiles();
	const existing = files[notePath];
	if (existing !== undefined) {
		try {
			// Overwrites in place — never re-create an existing mapping, or SAF
			// would uniquify the name into `note (1).md` (see module header).
			await fs.writeTextFile(existing, content);
			return true;
		} catch {
			// The mapped file is gone (user deleted/moved it) or the write was
			// refused; fall through and create a fresh file + mapping.
		}
	}
	try {
		// Creates missing parent directories recursively.
		const relPath = sanitizeRelPath(notePath);
		const mime = relPath.endsWith('.excalidraw') ? 'application/json' : 'text/markdown';
		const created = await fs.createNewFile(dir.uri, relPath, mime);
		await fs.writeTextFile(created, content);
		files[notePath] = created;
		writeJson(MIRROR_FILES_KEY, files);
		return true;
	} catch (err) {
		if (!revokedWarned) {
			revokedWarned = true;
			console.warn(
				'note mirror: writing to the mirror folder failed (access may have been revoked)',
				err
			);
		}
		return false;
	}
}

/**
 * Write one note's content into the mirror folder. No-op unless a folder is
 * configured; never throws — a broken mirror must not take the editor down
 * with it (failures warn once, see `revokedWarned`).
 */
export async function writeMirrorFile(notePath: string, content: string): Promise<void> {
	const fs = await loadFs();
	if (fs === null) return;
	const dir = readMirrorDir();
	if (dir === null) return;
	await writeMirrorFileInner(fs, dir, notePath, content);
}

// Per-note trailing-debounce timers: rapid edits collapse into one write ~2s
// after the last change, keyed per note so editing note A never delays the
// pending write of note B.
const mirrorTimers = new Map<string, ReturnType<typeof setTimeout>>();

/**
 * Debounced mirror of a single note. `getContent` is invoked when the timer
 * fires (not when scheduled) so the write always captures the latest text.
 * Cheap sync `IS_APP` gate up front — on the website build this is called on
 * every doc update and must cost nothing (no dynamic import, no timers).
 */
export function scheduleMirror(notePath: string, getContent: () => string): void {
	if (!IS_APP) return;
	const pending = mirrorTimers.get(notePath);
	if (pending !== undefined) clearTimeout(pending);
	mirrorTimers.set(
		notePath,
		setTimeout(() => {
			mirrorTimers.delete(notePath);
			void writeMirrorFile(notePath, getContent());
		}, MIRROR_DEBOUNCE_MS)
	);
}

/**
 * Mirror every note in the offline synced-notes registry by loading its
 * y-indexeddb copy directly (no websocket, no editor): the same
 * `Y.Doc` + `IndexeddbPersistence` + `whenSynced` pattern `connectSession`
 * uses, minus the provider. Runs sequentially — SAF document ops go through a
 * slow Binder round-trip each, and hammering them in parallel mostly causes
 * ANR-adjacent jank, not speedup. Per-note failures are counted and skipped so
 * one corrupt local doc can't abort the whole pass.
 */
export async function mirrorAllSyncedNotes(
	onProgress?: (done: number, total: number) => void
): Promise<{ written: number; failed: number }> {
	const fs = await loadFs();
	if (fs === null) return { written: 0, failed: 0 };
	const dir = readMirrorDir();
	if (dir === null) return { written: 0, failed: 0 };

	const paths = listSyncedNotes();
	let written = 0;
	let failed = 0;
	onProgress?.(0, paths.length);
	for (const path of paths) {
		const doc = new Y.Doc();
		const persistence = new IndexeddbPersistence(`${LOCAL_NOTE_DB_PREFIX}${path}`, doc);
		try {
			// "Local copy loaded", same as connectSession's whenLocalLoaded.
			await persistence.whenSynced;
			const content = doc.getText('content').toString();
			if (await writeMirrorFileInner(fs, dir, path, content)) written++;
			else failed++;
		} catch (err) {
			failed++;
			console.warn(`note mirror: failed to load local copy of ${path}`, err);
		} finally {
			try {
				await persistence.destroy();
			} catch {
				// already closed / never opened — nothing to release
			}
			doc.destroy();
		}
		onProgress?.(written + failed, paths.length);
	}
	return { written, failed };
}

// --- Startup prompt ----------------------------------------------------------

/**
 * Whether the startup dialog should be shown: app build, user never dismissed
 * it, and no folder has ever been chosen. `revoked` deliberately does NOT
 * re-trigger the modal — the settings page surfaces that state instead of
 * ambushing the user with a picker at launch.
 */
export async function shouldPromptForMirror(): Promise<boolean> {
	if (!IS_APP) return false;
	try {
		if (localStorage.getItem(MIRROR_DISMISSED_KEY) === '1') return false;
	} catch {
		// localStorage unavailable — fall through and let the state decide
	}
	return (await getMirrorState()).status === 'unset';
}

/** Remember that the user declined the startup prompt ("Not now"). */
export function dismissMirrorPrompt(): void {
	try {
		localStorage.setItem(MIRROR_DISMISSED_KEY, '1');
	} catch {
		// best-effort only
	}
}

/**
 * Logout cleanup: drop the file-uri map and the prompt dismissal, but keep
 * `rustnote_mirror_dir` — the folder grant and the mirrored files are the
 * user's own storage, chosen by them, and logging out of the app doesn't
 * revoke their access to their own folder. (The file map must go regardless:
 * logout wipes the local note docs, and a next login could be a different
 * account whose notes must not silently overwrite the old mapping's files.)
 */
export function clearMirrorLocalState(): void {
	removeKey(MIRROR_FILES_KEY);
	removeKey(MIRROR_DISMISSED_KEY);
}
