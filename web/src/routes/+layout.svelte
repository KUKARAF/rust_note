<script lang="ts">
	import '$lib/design/tokens.css';
	import favicon from '$lib/assets/favicon.svg';
	import { resolve } from '$app/paths';
	import { onMount } from 'svelte';
	import { auth, loadUser } from '$lib/stores/auth';
	import { settings, loadSettings } from '$lib/stores/settings';
	import { API_BASE_URL } from '$lib/api/client';
	import Button from '$lib/design/Button.svelte';
	import BlinkingCursor from '$lib/design/BlinkingCursor.svelte';

	let { children } = $props();

	onMount(() => {
		void loadUser();
		void loadSettings();
	});

	// Keep the DOM's data-theme in sync with the resolved theme (mirrors the
	// app.html pre-paint script, which only handles the initial/cached value).
	$effect(() => {
		document.documentElement.dataset.theme = $settings.theme;
	});

	function logout() {
		// Navigate (not fetch) so the backend's redirect response is followed
		// by the browser and any session cookie clearing happens as part of a
		// normal top-level navigation.
		window.location.href = `${API_BASE_URL}/auth/logout`;
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
			<span class="auth-status"
				>{$auth.user.display_name ?? $auth.user.email ?? 'Signed in'}</span
			>
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
