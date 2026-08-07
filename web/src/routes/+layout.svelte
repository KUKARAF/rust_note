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
	import { shouldPromptForMirror, clearMirrorLocalState } from '$lib/app/noteMirror';
	import MirrorFolderDialog from '$lib/app/MirrorFolderDialog.svelte';
	import CommandPalette from '$lib/commandPalette/CommandPalette.svelte';
	import { openTodayNote } from '$lib/notes/daily';
	import Button from '$lib/design/Button.svelte';
	import BlinkingCursor from '$lib/design/BlinkingCursor.svelte';
	import { get } from 'svelte/store';

	let { children } = $props();

	let paletteOpen = $state(false);

	function onWindowKeydown(event: KeyboardEvent) {
		// Global palette shortcut. The palette itself stops propagation of its
		// own keys, so this only ever sees Ctrl/Cmd+K "from outside".
		if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === 'k') {
			event.preventDefault();
			paletteOpen = !paletteOpen;
		}
	}

	// Touch gestures, armed by WHERE the touch starts so they never fight the
	// note list's / CodeMirror's own scrolling or pull-to-refresh:
	//  * swipe DOWN starting in the top region → command palette;
	//  * horizontal swipe starting at EITHER screen edge → today's daily note
	//    (edge-gating also avoids CodeMirror's horizontal scroll — a touch
	//    can't start ON the edge strips).
	// Registered manually with { passive: false } — NOT via <svelte:window>
	// attributes, which Svelte registers passively for touch events. Passive
	// listeners were why gestures did nothing on Android: the WebView claims
	// the gesture for scrolling almost immediately, fires touchcancel, and
	// our thresholds never got a chance. With passive:false we can
	// preventDefault() once a gesture arms, keeping the event stream alive.
	// Thresholds: dominant axis > 70px, cross axis < 40px, within 600ms.
	const SWIPE_TOP_REGION_PX = 140;
	const SWIPE_EDGE_REGION_PX = 40;
	/** Movement (px) along the armed axis after which we claim the gesture. */
	const SWIPE_CLAIM_PX = 10;
	let swipeStart: { x: number; y: number; at: number; kind: 'down' | 'left' | 'right' } | null =
		null;

	function onTouchStart(event: TouchEvent) {
		const touch = event.touches.item(0);
		swipeStart = null;
		if (event.touches.length !== 1 || touch === null) return;
		const start = { x: touch.clientX, y: touch.clientY, at: Date.now() };
		if (touch.clientX >= window.innerWidth - SWIPE_EDGE_REGION_PX) {
			swipeStart = { ...start, kind: 'left' };
		} else if (touch.clientX <= SWIPE_EDGE_REGION_PX) {
			// Left edge overlaps Android's back-gesture zone; the right edge is
			// the primary trigger, this one is best-effort.
			swipeStart = { ...start, kind: 'right' };
		} else if (touch.clientY <= SWIPE_TOP_REGION_PX) {
			swipeStart = { ...start, kind: 'down' };
		}
	}

	function onTouchMove(event: TouchEvent) {
		if (swipeStart === null || paletteOpen) return;
		const touch = event.touches.item(0);
		if (touch === null || event.touches.length !== 1) return;
		if (Date.now() - swipeStart.at > 600) {
			swipeStart = null;
			return;
		}
		const dx = touch.clientX - swipeStart.x;
		const dy = touch.clientY - swipeStart.y;
		const alongAxis = swipeStart.kind === 'down' ? dy : swipeStart.kind === 'left' ? -dx : dx;
		// Once the touch clearly moves along the armed axis, claim the gesture
		// from the WebView so it doesn't turn into a scroll and cancel us.
		if (alongAxis > SWIPE_CLAIM_PX && event.cancelable) {
			event.preventDefault();
		}
		if (swipeStart.kind === 'down' && dy > 70 && Math.abs(dx) < 40) {
			swipeStart = null;
			paletteOpen = true;
		} else if (swipeStart.kind !== 'down' && alongAxis > 70 && Math.abs(dy) < 40) {
			swipeStart = null;
			void openTodayNote();
		}
	}

	function onTouchEnd() {
		swipeStart = null;
	}

	onMount(() => {
		window.addEventListener('touchstart', onTouchStart, { passive: true });
		window.addEventListener('touchmove', onTouchMove, { passive: false });
		window.addEventListener('touchend', onTouchEnd, { passive: true });
		window.addEventListener('touchcancel', onTouchEnd, { passive: true });
		return () => {
			window.removeEventListener('touchstart', onTouchStart);
			window.removeEventListener('touchmove', onTouchMove);
			window.removeEventListener('touchend', onTouchEnd);
			window.removeEventListener('touchcancel', onTouchEnd);
		};
	});

	let mirrorPromptOpen = $state(false);
	// The prompt check should run exactly once per app start, the first time a
	// logged-in user materializes (whether via a fresh /auth/me or the offline
	// cache) — not again on later store updates. Fire-and-forget so startup
	// never blocks on the (dynamically imported) android-fs plugin.
	let mirrorPromptChecked = false;
	$effect(() => {
		if (!IS_APP || mirrorPromptChecked || $auth.user === null) return;
		mirrorPromptChecked = true;
		void shouldPromptForMirror().then((prompt) => {
			if (prompt) mirrorPromptOpen = true;
		});
	});

	onMount(() => {
		// App build: the backend finishes the OIDC flow by redirecting to
		// `http://tauri.localhost/#token=<device token>`. Capture it BEFORE
		// loadUser() so the very first /auth/me already carries the bearer
		// header, scrub it from the URL (it's a long-lived credential — keep it
		// out of the address bar/history), then land on the notes list.
		const capturedToken = IS_APP && window.location.hash.startsWith('#token=');
		if (capturedToken) {
			setDeviceToken(window.location.hash.slice('#token='.length));
			history.replaceState(null, '', window.location.pathname);
			void goto(resolve('/notes'));
		}
		void loadUser().then(() => {
			// Startup auth redirect (web and app alike). Skipped when a token
			// was just captured — that path already routed to /notes above.
			// /login must not bounce to itself, and /shared/* must stay
			// reachable logged-out (share links are the guest feature). Offline
			// note: loadUser falls back to the cached user on network failure,
			// so an offline start with a previous login does NOT end up here
			// with user === null — only genuinely signed-out states redirect.
			if (capturedToken) return;
			const path = window.location.pathname;
			if (path === '/login' || path === '/shared' || path.startsWith('/shared/')) return;
			if (get(auth).user === null) {
				void goto(resolve('/login'));
			} else if (path === '/') {
				// Logged in on the landing blurb: go straight to the notes list.
				void goto(resolve('/notes'));
			}
		});
		void loadSettings();

		// Safe-area fallback (app only): the edge-to-edge plugin is supposed to
		// inject --safe-area-inset-* on documentElement, but on at least one
		// tested device it doesn't. If the top inset is still unset/zero after
		// the webview settles, apply a conservative estimate so content clears
		// the status bar rather than rendering underneath it.
		if (IS_APP) {
			setTimeout(() => {
				const injected = getComputedStyle(document.documentElement)
					.getPropertyValue('--safe-area-inset-top')
					.trim();
				if (injected === '' || injected === '0px' || injected === '0') {
					document.documentElement.style.setProperty('--safe-area-inset-top', '32px');
					document.documentElement.style.setProperty('--safe-area-inset-bottom', '16px');
				}
			}, 500);
		}
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
			// Drops the mirror's file map + prompt dismissal, but keeps the
			// folder grant — the mirrored files are the user's own storage.
			clearMirrorLocalState();
			window.location.href = '/';
		}
	}
</script>

<svelte:head>
	<link rel="icon" href={favicon} />
</svelte:head>

<svelte:window
	onkeydown={onWindowKeydown}
	ontouchstart={onTouchStart}
	ontouchmove={onTouchMove}
	ontouchend={onTouchEnd}
/>

<div class="app-shell" class:app-mode={IS_APP}>
	{#if !IS_APP}
		<!-- The app build has no top bar at all: navigation, login and logout
		     live in the command palette (floating button / gestures below). -->
		<nav class="app-nav">
			<a class="app-name" href={resolve('/notes')}>rust_note<BlinkingCursor /></a>

			<div class="app-nav-spacer"></div>

			{#if $auth.loading}
				<span class="auth-status">…</span>
			{:else if $auth.user}
				<span class="auth-status">{$auth.user.display_name ?? $auth.user.email ?? 'Signed in'}</span
				>
				<Button variant="outline" size="sm" onclick={logout}>Log out</Button>
			{:else}
				<a class="login-link" href={resolve('/login')}>Log in</a>
			{/if}
		</nav>
	{/if}

	<main class="app-content">
		{@render children()}
	</main>
</div>

{#if IS_APP}
	<!-- Always-visible palette trigger: with no top bar, the palette is the
	     app's only navigation surface, and gestures alone are too fragile to
	     be the sole way in. -->
	<button
		type="button"
		class="palette-fab"
		aria-label="Open command palette"
		onclick={() => (paletteOpen = true)}
	>
		&gt;_
	</button>
{/if}

<MirrorFolderDialog open={mirrorPromptOpen} onclose={() => (mirrorPromptOpen = false)} />
<CommandPalette open={paletteOpen} onclose={() => (paletteOpen = false)} onlogout={logout} />

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
		/* Keep the nav clear of the Android status bar / display cutout. */
		padding-top: calc(var(--space-5) + var(--safe-top));
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
		/* Keep content clear of the Android gesture-nav / home-indicator area. */
		padding-bottom: calc(var(--screen-gutter) + var(--safe-bottom));
	}

	/* App build: no top bar, so the content itself must clear the status bar. */
	.app-shell.app-mode .app-content {
		padding-top: calc(var(--screen-gutter) + var(--safe-top));
	}

	.palette-fab {
		position: fixed;
		right: calc(var(--screen-gutter) + var(--safe-right));
		bottom: calc(var(--screen-gutter) + var(--safe-bottom) + var(--space-4));
		z-index: 90;
		background: var(--surface-card);
		color: var(--kv-accent);
		border: 1px solid var(--border-accent);
		border-radius: var(--radius-card);
		font-family: var(--font-pixel);
		font-size: var(--type-label);
		padding: 12px 14px;
		min-width: var(--tap-target);
		min-height: var(--tap-target);
		cursor: pointer;
		text-shadow: var(--glow-accent);
	}
</style>
