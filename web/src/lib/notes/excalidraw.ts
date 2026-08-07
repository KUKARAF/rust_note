// Excalidraw drawing support (view-first).
//
// The user's vault contains drawings written by the Obsidian excalidraw
// plugin as `Something.excalidraw.md`: a markdown wrapper with frontmatter,
// a `# Text Elements` / `## Text Elements` section (searchable text), and
// the actual scene under a `# Drawing` / `## Drawing` heading as either a
// ```json fence (older files) or a ```compressed-json fence (newer files,
// LZ-String compressToBase64 chunked at 256 chars with blank lines between
// chunks — see the plugin's sceneDataUtils.ts). Both variants exist in the
// vault, so both are handled here. A bare `.excalidraw` file (no markdown
// wrapper, pure JSON) is also parsed for future-proofing, though the server
// only lists `.md` files today.
//
// This module is pure text-in/object-out; rendering lives in
// ExcalidrawView.svelte (official @excalidraw/utils exportToSvg, loaded
// lazily — it is browser-only and heavyweight).

import LZString from 'lz-string';

/** Minimal structural view of an Excalidraw scene — not the full types. */
export interface ExcalidrawScene {
	elements: unknown[];
	appState?: Record<string, unknown>;
	files?: Record<string, unknown>;
}

/** Drawing notes: the `.md` wrapper strips to an id ending in `.excalidraw`. */
export function isExcalidrawNote(noteId: string): boolean {
	return noteId.endsWith('.excalidraw');
}

/**
 * Seed content for a freshly created drawing: a bare pretty-printed scene in
 * the same shape the server's migration produces (and `serializeAsJSON`
 * writes back). Pretty-printed + trailing newline so the on-disk file diffs
 * cleanly in git, like every other note.
 */
export const EMPTY_SCENE =
	JSON.stringify(
		{
			type: 'excalidraw',
			version: 2,
			source: 'rust_note',
			elements: [],
			appState: {},
			files: {}
		},
		null,
		2
	) + '\n';

/**
 * Matches the scene block: `# Drawing` or `## Drawing`, then a ```json or
 * ```compressed-json fence. Tolerates blank lines between heading and fence.
 */
const DRAWING_BLOCK_RE = /(?:^|\n)#{1,2} Drawing\s*\n+```(compressed-json|json)\n([\s\S]*?)\n```/;

function parseSceneObject(raw: string): ExcalidrawScene | null {
	try {
		const parsed: unknown = JSON.parse(raw);
		if (parsed === null || typeof parsed !== 'object') return null;
		const scene = parsed as Record<string, unknown>;
		if (!Array.isArray(scene.elements)) return null;
		return {
			elements: scene.elements,
			appState: (scene.appState as Record<string, unknown> | undefined) ?? {},
			files: (scene.files as Record<string, unknown> | undefined) ?? {}
		};
	} catch {
		return null;
	}
}

/**
 * Extract the Excalidraw scene from a note's content. Returns null when the
 * content has no parseable drawing — callers fall back to the text editor.
 */
export function parseExcalidrawScene(content: string): ExcalidrawScene | null {
	const trimmed = content.trim();

	// Bare `.excalidraw` file: the whole content is the scene JSON.
	if (trimmed.startsWith('{')) {
		return parseSceneObject(trimmed);
	}

	const match = DRAWING_BLOCK_RE.exec(content);
	if (match === null) return null;
	const fence = match[1];
	const block = match[2] ?? '';

	if (fence === 'json') {
		return parseSceneObject(block);
	}

	// compressed-json: LZ-String base64, wrapped across lines — strip ALL
	// whitespace before decompressing (the plugin chunks at 256 chars).
	const compact = block.replace(/\s+/g, '');
	const json = LZString.decompressFromBase64(compact);
	if (json === null || json === '') return null;
	return parseSceneObject(json);
}
