// Note ids sourced from the filesystem (rather than created via the app's
// slugify_note_id) can contain characters like '#', '?', '%', or '&' that
// are meaningful to the URL parser. Re-encode each path segment before
// building an href/goto target so such ids round-trip correctly instead of
// being truncated (at '#'/'?') or otherwise misinterpreted.
export function encodeNotePath(path: string): string {
	return path.split('/').map(encodeURIComponent).join('/');
}
