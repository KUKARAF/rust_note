// Toggle a todo's checkbox in its source note and persist it through the
// collab CRDT. A REST PUT is deliberately NOT used: for any note with a live
// collab room the room's debounced flush would clobber the PUT, and connected
// editors would never see the change. Instead we open a short-lived Yjs
// session (exactly what the editor does), flip the single `[ ]`/`[x]`
// character in one transaction, and let the server's debounced flush commit
// it — the same pattern as $lib/notes/TrackValueDialog.svelte.

import type { WebsocketProvider } from 'y-websocket';
import { createCollabSession, deriveUserColor, type CollabUser } from '$lib/editor/collabProvider';
import type { AuthUser } from '$lib/stores/auth';
import type { Todo } from '$lib/notes/todos';

/** Build the collab identity (caret label + color) from the auth user. */
export function collabUserFrom(user: AuthUser): CollabUser {
	return {
		id: user.id,
		name: user.display_name ?? user.email ?? 'me',
		color: deriveUserColor(user.id)
	};
}

const delay = (ms: number) => new Promise<void>((r) => setTimeout(r, ms));

/** Resolve once the provider reports a completed sync, or after `timeoutMs`. */
function waitForSync(provider: WebsocketProvider, timeoutMs: number): Promise<void> {
	return new Promise((resolve) => {
		if (provider.synced) {
			resolve();
			return;
		}
		let settled = false;
		const finish = () => {
			if (settled) return;
			settled = true;
			provider.off('sync', onSync);
			resolve();
		};
		const onSync = (isSynced: boolean) => {
			if (isSynced) finish();
		};
		provider.on('sync', onSync);
		setTimeout(finish, timeoutMs);
	});
}

/** Column of the checkbox marker char on a task line, or null if not a task. */
function checkboxCol(line: string): number | null {
	const m = line.match(/^(\s*[-*+][ \t]+\[)[ xXoOmM]\]/);
	return m ? m[1].length : null;
}

/** The task text on a line (everything after the `]`, left-trimmed). */
function lineTaskText(line: string, col: number): string {
	return line.slice(col + 2).trimStart();
}

/** Byte-independent char offset of the start of line `i` in `text`. */
function lineStartOffset(lines: string[], i: number): number {
	let off = 0;
	for (let j = 0; j < i; j++) off += lines[j].length + 1; // +1 for the '\n'
	return off;
}

/**
 * Absolute char offset of `todo`'s checkbox marker within `text`. Prefers the
 * reported line but verifies the task text still matches; falls back to the
 * first line whose task text matches, so an edit above it doesn't misfire.
 */
function locateCheckbox(text: string, todo: Todo): number | null {
	const lines = text.split('\n');
	const want = todo.text.trim();

	const at = (i: number, requireTextMatch: boolean): number | null => {
		if (i < 0 || i >= lines.length) return null;
		const col = checkboxCol(lines[i]);
		if (col === null) return null;
		if (requireTextMatch && lineTaskText(lines[i], col).trim() !== want) return null;
		return lineStartOffset(lines, i) + col;
	};

	const primary = at(todo.line - 1, true);
	if (primary !== null) return primary;

	for (let i = 0; i < lines.length; i++) {
		const hit = at(i, true);
		if (hit !== null) return hit;
	}
	return null;
}

/**
 * Set `todo`'s done state to `want` in its note. Resolves when the edit has
 * been applied and given a moment to flush; throws an Error with a
 * user-facing message on failure (task not found, offline, etc.).
 */
export async function setTodoDone(todo: Todo, user: CollabUser, want: boolean): Promise<void> {
	const session = createCollabSession(todo.note_id, user);
	try {
		// Wait for the local copy AND server convergence before computing the
		// offset — this note may not be the one on screen, so its doc starts
		// empty and must catch up first.
		await session.whenLocalLoaded;
		await waitForSync(session.provider, 4000);

		const text = session.ytext.toString();
		const pos = locateCheckbox(text, todo);
		if (pos === null) {
			throw new Error("Couldn't find this task in its note — it may have been edited.");
		}

		const current = text[pos];
		const alreadyDone = current === 'x' || current === 'X';
		if (alreadyDone === want) return; // nothing to do

		const doc = session.ytext.doc;
		const apply = () => {
			session.ytext.delete(pos, 1);
			session.ytext.insert(pos, want ? 'x' : ' ');
		};
		if (doc) doc.transact(apply);
		else apply();

		// Keep the socket open briefly so the update frame reaches the server;
		// its own debounced task then commits to git even after we disconnect.
		await delay(900);
	} finally {
		session.destroy();
	}
}
