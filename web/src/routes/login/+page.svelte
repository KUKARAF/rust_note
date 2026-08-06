<script lang="ts">
	import { API_BASE_URL } from '$lib/api/client';
	import { IS_APP } from '$lib/api/deviceToken';
	import { auth } from '$lib/stores/auth';
	import { goto } from '$app/navigation';
	import { resolve } from '$app/paths';
	import Card from '$lib/design/Card.svelte';
	import Button from '$lib/design/Button.svelte';

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
		justify-content: center;
		padding-top: var(--space-10);
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
