<script lang="ts">
	import '$lib/design/tokens.css';
	import favicon from '$lib/assets/favicon.svg';
	import { resolve } from '$app/paths';
	import { goto } from '$app/navigation';
	import { onMount } from 'svelte';
	import { auth, loadUser } from '$lib/stores/auth';
	import { settings, loadSettings } from '$lib/stores/settings';
	import { apiPost } from '$lib/api/client';
	import { IS_APP, setDeviceToken, clearDeviceToken } from '$lib/api/deviceToken';
	import { clearAllLocalNotes } from '$lib/editor/collabProvider';
	import {
		clearSyncedRegistry,
		clearNotesListCache,
		clearAllNoteMeta,
		clearCachedUser
	} from '$lib/stores/offline';
	import Button from '$lib/design/Button.svelte';
	import BlinkingCursor from '$lib/design/BlinkingCursor.svelte';

	let { children } = $props();

	onMount(() => {
		// App build: the backend finishes the OIDC flow by redirecting to
		// `http://tauri.localhost/#token=<device token>`. Capture it BEFORE
		// loadUser() so the very first /auth/me already carries the bearer
		// header, scrub it from the URL (it's a long-lived credential — keep it
		// out of the address bar/history), then land on the notes list.
		if (IS_APP && window.location.hash.startsWith('#token=')) {
			setDeviceToken(window.location.hash.slice('#token='.length));
			history.replaceState(null, '', window.location.pathname);
			void goto(resolve('/notes'));
		}
		void loadUser();
		void loadSettings();
	});

	// Keep the DOM's data-theme in sync with the resolved theme (mirrors the
	// app.html pre-paint script, which only handles the initial/cached value).
	$effect(() => {
		document.documentElement.dataset.theme = $settings.theme;
	});

	async function logout() {
		// POST via fetch: the route is POST-only on purpose (a GET logout
		// could be forced cross-site by e.g. an <img> tag or link
		// prefetching). Set-Cookie on a fetch response clears the session
		// cookie just as well (and on the app build the POST revokes the
		// device token server-side); the hard navigation afterwards resets
		// all client state regardless of whether the request succeeded.
		try {
			await apiPost('/auth/logout');
		} finally {
			// Wipe everything this device knows, so a shared machine keeps no
			// readable note content or identity around after logout.
			clearDeviceToken();
			try {
				// Must run before clearSyncedRegistry(): without
				// indexedDB.databases() the DB names come from that registry.
				await clearAllLocalNotes();
			} catch (err) {
				console.error('Failed to clear local note copies', err);
			}
			clearSyncedRegistry();
			clearNotesListCache();
			clearAllNoteMeta();
			clearCachedUser();
			window.location.href = '/';
		}
	}
</script>

<svelte:head>
	<link rel="icon" href={favicon} />
</svelte:head>

<div class="app-shell">
	<nav class="app-nav">
		<a class="app-name" href={resolve('/notes')}>rust_note<BlinkingCursor /></a>

		<div class="app-nav-spacer"></div>

		{#if $auth.loading}
			<span class="auth-status">…</span>
		{:else if $auth.user}
			<span class="auth-status">{$auth.user.display_name ?? $auth.user.email ?? 'Signed in'}</span>
			<Button variant="outline" size="sm" onclick={logout}>Log out</Button>
		{:else}
			<a class="login-link" href={resolve('/login')}>Log in</a>
		{/if}
	</nav>

	<main class="app-content">
		{@render children()}
	</main>
</div>

<style>
	.app-shell {
		display: flex;
		flex-direction: column;
		min-height: 100vh;
		background: var(--kv-bg);
	}

	.app-nav {
		display: flex;
		align-items: center;
		gap: var(--space-6);
		padding: var(--space-5) var(--screen-gutter);
		background: var(--surface-card);
		border-bottom: 1px solid var(--border-default);
	}

	.app-name {
		font-family: var(--font-pixel);
		font-size: var(--type-title);
		text-transform: uppercase;
		letter-spacing: var(--tracking-pixel);
		color: var(--kv-accent);
		text-shadow: var(--glow-accent);
		text-decoration: none;
		display: inline-flex;
		align-items: baseline;
		gap: 2px;
	}

	.app-nav-spacer {
		flex: 1;
	}

	.auth-status {
		font-family: var(--font-term);
		font-size: var(--type-meta);
		color: var(--kv-dim);
	}

	.login-link {
		font-family: var(--font-pixel);
		font-size: var(--type-label);
		text-transform: uppercase;
		letter-spacing: var(--tracking-pixel);
		color: var(--kv-accent);
		text-decoration: none;
		border: 1px solid var(--border-accent);
		border-radius: var(--radius-control);
		padding: 8px 12px;
		transition: opacity 120ms linear;
	}

	.login-link:hover {
		opacity: 0.82;
	}

	.app-content {
		flex: 1;
		padding: var(--screen-gutter);
	}
</style>
