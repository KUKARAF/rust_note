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
import type { Awareness } from 'y-protocols/awareness';
import { API_BASE_URL } from '$lib/api/client';

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
	providerOptions: ConstructorParameters<typeof WebsocketProvider>[3] = {}
): CollabSession {
	const doc = new Y.Doc();
	// Bind to the exact field name the backend seeds and the editor reads.
	const ytext = doc.getText('content');

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
		destroy() {
			if (destroyed) return;
			destroyed = true;
			undoManager.destroy();
			// Removes local awareness state, closes the socket, unsubscribes bc.
			provider.destroy();
			doc.destroy();
		}
	};
}

export function createCollabSession(noteId: string, user: CollabUser): CollabSession {
	return connectSession(collabWsBaseUrl(), encodeRoomName(noteId), user, {
		// Cookies/session ride along automatically on same-origin; in the
		// cross-origin dev setup the session isn't required (every connection is
		// `admin`). Standard provider config — no custom URL munging needed.
		// (BroadcastChannel left enabled so multiple tabs on the same origin
		// sync instantly without a server round-trip.)
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
	// ever changes.
	return connectSession(collabSharedWsBaseUrl(), encodeURIComponent(token), user, {});
}
