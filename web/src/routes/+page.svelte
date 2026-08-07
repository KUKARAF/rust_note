<script lang="ts">
	// `/` is a pure dispatcher, not a page: there is nothing to do at the root
	// except go to the notes (logged in) or the login screen (logged out).
	// The layout's startup redirect covers the initial load; this effect also
	// covers in-app navigations back to `/` (e.g. after logout's hard reload).
	import { goto } from '$app/navigation';
	import { resolve } from '$app/paths';
	import { auth } from '$lib/stores/auth';

	$effect(() => {
		if ($auth.loading) return;
		void goto(resolve($auth.user ? '/notes' : '/login'), { replaceState: true });
	});
</script>

<p class="index-redirect">…</p>

<style>
	.index-redirect {
		text-align: center;
		font-family: var(--font-term);
		color: var(--kv-dim);
		padding: var(--space-10) 0;
	}
</style>
