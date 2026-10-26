// Folder derivation from the flat NoteMeta list. Folders have no backend
// entity — they are path prefixes inferred from note IDs at list time.

import type { NoteMeta } from '$lib/commandPalette/items';

/** Immediate sub-folder full paths under `parentFolder` ('' = root). */
export function getSubFolders(notes: NoteMeta[], parentFolder: string): string[] {
	const prefix = parentFolder ? parentFolder + '/' : '';
	const seen = new Set<string>();
	for (const note of notes) {
		if (prefix && !note.id.startsWith(prefix)) continue;
		const rest = note.id.slice(prefix.length);
		const slash = rest.indexOf('/');
		if (slash !== -1) {
			seen.add(prefix + rest.slice(0, slash));
		}
	}
	return [...seen].sort();
}

/** Notes that are direct children of `parentFolder` (no further `/`). */
export function getDirectNotes(notes: NoteMeta[], parentFolder: string): NoteMeta[] {
	if (parentFolder === '') {
		return notes.filter((n) => !n.id.includes('/'));
	}
	const prefix = parentFolder + '/';
	return notes.filter((n) => n.id.startsWith(prefix) && !n.id.slice(prefix.length).includes('/'));
}

/** Breadcrumb segments for a folder path (includes root "Vault" entry). */
export function getBreadcrumbs(folder: string): Array<{ label: string; path: string }> {
	const crumbs: Array<{ label: string; path: string }> = [{ label: 'Vault', path: '' }];
	if (!folder) return crumbs;
	const segments = folder.split('/');
	for (let i = 0; i < segments.length; i++) {
		crumbs.push({
			label: segments[i].replace(/-/g, ' '),
			path: segments.slice(0, i + 1).join('/')
		});
	}
	return crumbs;
}

/**
 * Minimal slug that mirrors the server's slugify_note_id rule: lowercase,
 * non-alphanumeric runs → single hyphen, trim leading/trailing hyphens.
 * Slashes in the input are preserved so nested paths ("work/meetings") work.
 */
export function slugifySegment(raw: string): string {
	return raw
		.trim()
		.toLowerCase()
		.replace(/[^a-z0-9/]+/g, '-')
		.replace(/^-+|-+$/g, '')
		.replace(/\/-+/g, '/')
		.replace(/-+\//g, '/');
}
