import type { PageLoad } from './$types';

// This route has a dynamic (uncrawlable) param, so it can't be prerendered
// at build time. It's still served fine at runtime via adapter-static's SPA
// fallback (see the `fallback` option in vite.config.ts).
export const prerender = false;

// Catch-all route param: the note's vimwiki-relative path, e.g. "journal/2026-08-01".
export const load: PageLoad = ({ params }) => {
	return {
		path: params.path
	};
};
