// Real-time collaboration session: a Yjs `Y.Doc` synced to the backend's
// collaboration endpoint via a `y-websocket` `WebsocketProvider`, plus the
// pieces the CodeMirror editor (y-codemirror.next) and the presence UI need.
//
// The backend seeds each room from the note's on-disk content and persists
// edits automatically (debounced git commit ~5s after typing stops), so the
// client no longer issues REST PUTs — the CRDT is the source of truth once a
// room is joined.

import * as Y from 'yjs';
import { WebsocketProvider } from 'y-websocket';
import { IndexeddbPersistence } from 'y-indexeddb';
import type { Awareness } from 'y-protocols/awareness';
import { API_BASE_URL } from '$lib/api/client';
import { getDeviceToken, IS_APP } from '$lib/api/deviceToken';
import { listSyncedNotes } from '$lib/stores/offline';
// Note: noteMirror imports LOCAL_NOTE_DB_PREFIX back from this module — a
// benign cycle, since both sides only touch the other's exports at call time
// (never during module evaluation). `scheduleMirror` itself is a cheap sync
// gate on the website build; the android-fs plugin is only dynamically
// imported inside it on the app build.
import { scheduleMirror } from '$lib/app/noteMirror';

/** Identity used to render this client's caret/label and presence chip. */
export interface CollabUser {
	id: string;
	name: string;
	color: string;
}

export interface CollabSession {
	doc: Y.Doc;
	provider: WebsocketProvider;
	/** The shared text bound to the editor. */
	ytext: Y.Text;
	/** Undo/redo scoped to local edits (see y-codemirror.next). */
	undoManager: Y.UndoManager;
	awareness: Awareness;
	/**
	 * Local IndexedDB persistence of the doc (offline-first). Only present for
	 * authed sessions — guest share links don't persist anything locally.
	 */
	persistence: IndexeddbPersistence | null;
	/**
	 * Resolves once the locally persisted copy of the doc (if any) has been
	 * loaded into `doc`. For sessions without persistence it is already
	 * resolved. Lets the editor open a previously-synced note while offline
	 * without waiting for the (unreachable) websocket.
	 */
	whenLocalLoaded: Promise<void>;
	/** Tear down the provider + doc + undo manager. Safe to call once. */
	destroy(): void;
}

/**
 * Deterministic per-user color, matching the server's scheme so a given user
 * has the same color everywhere: 32-bit FNV-1a of the id, `hue = hash % 360`.
 */
export function deriveUserColor(id: string): string {
	let hash = 2166136261 >>> 0; // FNV offset basis
	for (let i = 0; i < id.length; i++) {
		hash ^= id.charCodeAt(i);
		hash = Math.imul(hash, 16777619) >>> 0; // FNV prime, keep unsigned 32-bit
	}
	const hue = (hash >>> 0) % 360;
	return `hsl(${hue}, 65%, 55%)`;
}

/**
 * Base URL for the WebSocket collab endpoint, derived from the REST API base
 * (http -> ws, https -> wss). `WebsocketProvider` appends `/${roomName}`, so
 * this returns `<origin>/ws/notes`.
 */
export function collabWsBaseUrl(): string {
	const wsOrigin = API_BASE_URL.replace(/^http(s?):\/\//, (_m, s) => (s ? 'wss://' : 'ws://'));
	return `${wsOrigin.replace(/\/+$/, '')}/ws/notes`;
}

/**
 * Build the y-websocket room name for a note id.
 *
 * The backend route is a `{*note_id}` catch-all whose value may contain
 * slashes (nested notes, e.g. `projects/foo`). y-websocket v3 does NOT
 * re-encode the room name — it concatenates `serverUrl + '/' + roomName` — so
 * we encode each path SEGMENT individually (handling ids with spaces or other
 * unsafe characters) while leaving the `/` separators intact. axum then
 * percent-decodes each segment back to the original id. Verified live against
 * flat ids (`index`), nested ids (`diary/2026-06-24`), and ids containing
 * spaces (`Medical stuff/Untitled` -> `Medical%20stuff/Untitled`).
 */
export function encodeRoomName(noteId: string): string {
	return noteId.split('/').map(encodeURIComponent).join('/');
}

function connectSession(
	wsBaseUrl: string,
	roomName: string,
	user: CollabUser,
	persistenceName: string | null,
	providerOptions: ConstructorParameters<typeof WebsocketProvider>[3] = {}
): CollabSession {
	const doc = new Y.Doc();
	// Bind to the exact field name the backend seeds and the editor reads.
	const ytext = doc.getText('content');

	// Offline-first: mirror the doc into IndexedDB *before* connecting, so a
	// previously-synced note loads (and stays editable) with no network, and
	// edits made offline are stored locally until the websocket reconnects and
	// the CRDTs merge. `whenSynced` here means "local copy loaded", nothing to
	// do with the server.
	const persistence =
		persistenceName !== null ? new IndexeddbPersistence(persistenceName, doc) : null;
	const whenLocalLoaded: Promise<void> =
		persistence !== null ? persistence.whenSynced.then(() => undefined) : Promise.resolve();

	const provider = new WebsocketProvider(wsBaseUrl, roomName, doc, providerOptions);

	// Undo/redo tracking only local edits, shared with y-codemirror.next.
	const undoManager = new Y.UndoManager(ytext);

	// This is exactly what y-codemirror.next's cursor plugin reads to render
	// remote carets/selections with this user's color + name label.
	provider.awareness.setLocalStateField('user', { name: user.name, color: user.color });

	let destroyed = false;
	return {
		doc,
		provider,
		ytext,
		undoManager,
		awareness: provider.awareness,
		persistence,
		whenLocalLoaded,
		destroy() {
			if (destroyed) return;
			destroyed = true;
			undoManager.destroy();
			// Removes local awareness state, closes the socket, unsubscribes bc.
			provider.destroy();
			// Detaches from the doc and closes the DB connection — the stored
			// data itself is kept (that's the point; logout wipes it via
			// `clearAllLocalNotes`).
			void persistence?.destroy();
			doc.destroy();
		}
	};
}

/**
 * Prefix for the per-note IndexedDB database names used by y-indexeddb.
 * `clearAllLocalNotes()` matches on this prefix, so keep them in sync (the
 * Android notes-folder mirror also loads local docs by this prefix, see
 * `$lib/app/noteMirror`).
 */
export const LOCAL_NOTE_DB_PREFIX = 'rustnote-note:';

export function createCollabSession(noteId: string, user: CollabUser): CollabSession {
	// App build: websockets can't carry an Authorization header, so the device
	// token rides along as a `?token=` query param instead (y-websocket appends
	// `params` to the connection URL). On the website build there is no token
	// and cookies/session ride along automatically on same-origin; in the
	// cross-origin dev setup the session isn't required (every connection is
	// `admin`). (BroadcastChannel left enabled so multiple tabs on the same
	// origin sync instantly without a server round-trip.)
	const token = getDeviceToken();
	const session = connectSession(
		collabWsBaseUrl(),
		encodeRoomName(noteId),
		user,
		// Local persistence is keyed by the *raw* note path (same value the
		// offline synced-notes registry stores), not the encoded room name.
		`${LOCAL_NOTE_DB_PREFIX}${noteId}`,
		token !== null ? { params: { token } } : {}
	);

	// App build: keep the mirror folder's .md copy of this note fresh. Every
	// content change funnels through `doc.on('update')` (local typing, remote
	// edits, AND the initial y-indexeddb load), plus the provider's `sync`
	// event covers the "server had newer state" case where the initial merge
	// already fired updates before the mirror could matter. scheduleMirror
	// debounces per note (~2s trailing), so this firehose is cheap. Both
	// listeners are registered on the doc/provider themselves, so
	// `session.destroy()` (provider.destroy + doc.destroy) tears them down —
	// no dangling observers to clean up here.
	if (IS_APP) {
		const mirror = () => scheduleMirror(noteId, () => session.ytext.toString());
		session.provider.on('sync', (isSynced: boolean) => {
			if (isSynced) mirror();
		});
		session.doc.on('update', mirror);
	}

	return session;
}

/**
 * Delete every locally persisted note doc (the `rustnote-note:*` IndexedDB
 * databases). Called on logout so note content doesn't linger on a shared
 * device. `indexedDB.databases()` isn't universal (notably older Firefox), so
 * when unavailable the DB names are reconstructed from the offline store's
 * synced-notes registry — meaning this must run BEFORE the registry itself is
 * cleared.
 */
export async function clearAllLocalNotes(): Promise<void> {
	let names: string[];
	if (typeof indexedDB.databases === 'function') {
		const dbs = await indexedDB.databases();
		names = dbs
			.map((db) => db.name)
			.filter((name): name is string => !!name && name.startsWith(LOCAL_NOTE_DB_PREFIX));
	} else {
		names = listSyncedNotes().map((path) => `${LOCAL_NOTE_DB_PREFIX}${path}`);
	}
	await Promise.all(names.map(deleteIndexedDb));
}

function deleteIndexedDb(name: string): Promise<void> {
	return new Promise((resolve) => {
		const request = indexedDB.deleteDatabase(name);
		request.onsuccess = () => resolve();
		// Never block logout on a stubborn DB: on error just move on, and on
		// `blocked` (another tab still holds a connection) the deletion will
		// complete once that connection closes — no need to wait here.
		request.onerror = () => resolve();
		request.onblocked = () => resolve();
	});
}

/**
 * Base URL for the guest-share WebSocket endpoint (`/ws/shared/{token}`),
 * derived the same way as `collabWsBaseUrl()`. The token itself IS the
 * credential here — no cookie/session is sent or required.
 */
export function collabSharedWsBaseUrl(): string {
	const wsOrigin = API_BASE_URL.replace(/^http(s?):\/\//, (_m, s) => (s ? 'wss://' : 'ws://'));
	return `${wsOrigin.replace(/\/+$/, '')}/ws/shared`;
}

/**
 * Open a collab session for a public share link (`GET /ws/shared/{token}`).
 * No auth/cookie is needed — the token in the path is the credential. The
 * guest's awareness color is still derived deterministically (FNV-1a -> hue),
 * just hashed from a random per-session guest id since there's no stable
 * account id to key off of.
 */
export function createGuestCollabSession(token: string, user: CollabUser): CollabSession {
	// Tokens are opaque (no '/'), but encode defensively anyway in case that
	// ever changes. No local persistence: guests shouldn't leave note content
	// behind on whatever machine they opened the link on.
	return connectSession(collabSharedWsBaseUrl(), encodeURIComponent(token), user, null, {});
}
