<script lang="ts">
	import { onMount } from 'svelte';
	import { API_BASE_URL } from '$lib/api/client';
	import { IS_APP } from '$lib/api/deviceToken';
	import { auth, consumeLoginRequired } from '$lib/stores/auth';
	import { goto } from '$app/navigation';
	import { resolve } from '$app/paths';
	import Card from '$lib/design/Card.svelte';
	import Button from '$lib/design/Button.svelte';
	import Toast from '$lib/design/Toast.svelte';

	// True when the user was BOUNCED here by a 401 (rather than navigating to
	// the login page themselves) — shows a "login required" toast so the jump
	// isn't mysterious.
	let loginRequired = $state(false);
	onMount(() => {
		loginRequired = consumeLoginRequired();
	});

	// If we already know the user is logged in, don't show the login card —
	// bounce straight to the notes list.
	$effect(() => {
		if (!$auth.loading && $auth.user) {
			void goto(resolve('/notes'));
		}
	});

	function login() {
		// Full browser navigation (not a fetch): the backend handles the OIDC
		// flow itself and redirects back once authenticated. The app build asks
		// for the device-token variant (`?client=app`): instead of a session
		// cookie, the flow ends at `tauri.localhost/#token=<raw>` — see
		// $lib/api/deviceToken.
		window.location.href = `${API_BASE_URL}/auth/login${IS_APP ? '?client=app' : ''}`;
	}
</script>

<div class="login-wrap">
	{#if loginRequired}
		<div class="login-toast">
			<Toast message="login required" color="var(--kv-orange)" />
		</div>
	{/if}
	<Card class="login-card">
		<h1 class="login-title">rust_note</h1>
		<p class="login-desc">Sign in with your organization account to continue.</p>
		<div class="login-action">
			<Button variant="primary" size="lg" onclick={login}>Log in</Button>
		</div>
	</Card>
</div>

<style>
	.login-wrap {
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: var(--space-6);
		padding-top: var(--space-10);
	}

	.login-toast {
		display: flex;
		justify-content: center;
	}

	:global(.login-card) {
		max-width: 26rem;
		width: 100%;
		text-align: center;
	}

	.login-title {
		font-family: var(--font-pixel);
		font-size: var(--type-display);
		text-transform: uppercase;
		letter-spacing: var(--tracking-pixel);
		color: var(--kv-accent);
		text-shadow: var(--glow-accent);
		margin: 0 0 var(--space-7);
	}

	.login-desc {
		font-family: var(--font-term);
		font-size: var(--type-body);
		color: var(--kv-dim);
		margin: 0 0 var(--space-7);
	}

	.login-action {
		display: flex;
		justify-content: center;
	}
</style>
