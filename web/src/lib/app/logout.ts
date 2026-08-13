// Shared logout, used by the top-nav button (web) and the command palette's
// "Log out" action. Lives outside +layout.svelte so the palette-items module
// can invoke it directly without threading a callback through every consumer.

import { apiPost } from '$lib/api/client';
import { clearDeviceToken } from '$lib/api/deviceToken';
import { clearAllLocalNotes } from '$lib/editor/collabProvider';
import {
	clearSyncedRegistry,
	clearNotesListCache,
	clearAllNoteMeta,
	clearCachedUser
} from '$lib/stores/offline';
import { clearMirrorLocalState } from '$lib/app/noteMirror';

export async function logout(): Promise<void> {
	// POST via fetch: the route is POST-only on purpose (a GET logout could be
	// forced cross-site by e.g. an <img> tag or link prefetching). Set-Cookie on
	// a fetch response clears the session cookie just as well (and on the app
	// build the POST revokes the device token server-side); the hard navigation
	// afterwards resets all client state regardless of whether the request
	// succeeded.
	try {
		await apiPost('/auth/logout');
	} finally {
		// Wipe everything this device knows, so a shared machine keeps no
		// readable note content or identity around after logout.
		clearDeviceToken();
		try {
			// Must run before clearSyncedRegistry(): without indexedDB.databases()
			// the DB names come from that registry.
			await clearAllLocalNotes();
		} catch (err) {
			console.error('Failed to clear local note copies', err);
		}
		clearSyncedRegistry();
		clearNotesListCache();
		clearAllNoteMeta();
		clearCachedUser();
		// Drops the mirror's file map + prompt dismissal, but keeps the folder
		// grant — the mirrored files are the user's own storage.
		clearMirrorLocalState();
		window.location.href = '/';
	}
}
