import type { PageLoad } from './$types';

// See notes/[...path]/+page.ts for why this is not prerendered.
export const prerender = false;

export const load: PageLoad = ({ params }) => {
	return {
		token: params.token
	};
};
