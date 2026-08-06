// Placeholder module.
//
// Will contain [[wiki-link]] detection/parsing logic for vimwiki-format
// markdown: recognizing `[[path]]` and `[[path|title]]` links, resolving
// them to note paths, and (later) powering CodeMirror decorations/autocomplete.

/** Matches `[[link]]` and `[[link|title]]` wiki-link syntax. */
export const WIKI_LINK_PATTERN = /\[\[([^\]|]+)(?:\|([^\]]+))?\]\]/g;

export interface WikiLink {
	/** Raw target path/name inside the brackets, e.g. "journal/2026-08-01". */
	target: string;
	/** Optional display title, e.g. from `[[target|title]]`. */
	title?: string;
}

export function findWikiLinks(text: string): WikiLink[] {
	const links: WikiLink[] = [];
	for (const match of text.matchAll(WIKI_LINK_PATTERN)) {
		const [, target, title] = match;
		links.push({ target, title });
	}
	return links;
}
