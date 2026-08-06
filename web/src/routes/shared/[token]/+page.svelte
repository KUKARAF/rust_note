<script lang="ts">
	// Public guest view for a share link: `GET /api/shared/{token}` resolves
	// the token (no auth) to note metadata + first-paint content, then a live
	// collab session joins the SAME Yjs room the owner's `/notes/[...path]`
	// page connects to (`/ws/shared/{token}` <-> `/ws/notes/{noteId}` are two
	// doors into one room, byte-compatible y-websocket protocol either way).
	//
	// Guests have no account: identity is a random per-session id (used only
	// to derive a stable-for-this-session awareness color) plus an editable
	// display name defaulting to an auto-generated "Guest-XXXX".
	import type { PageProps } from './$types';
	import { apiGet, ApiError } from '$lib/api/client';
	import CodeMirrorEditor from '$lib/editor/CodeMirrorEditor.svelte';
	import CollabPresence, { type PresencePeer } from '$lib/editor/CollabPresence.svelte';
	import PulsingDot from '$lib/design/PulsingDot.svelte';
	import Chip from '$lib/design/Chip.svelte';
	import {
		createGuestCollabSession,
		deriveUserColor,
		type CollabSession
	} from '$lib/editor/collabProvider';

	interface SharedNoteResponse {
		note_id: string;
		title: string;
		permission: 'view' | 'edit';
		owner_display_name: string;
		content: string;
		expires_at: string | null;
	}

	let { data }: PageProps = $props();

	type LoadState = 'loading' | 'ok' | 'not-found' | 'error';
	type ConnStatus = 'connecting' | 'connected' | 'disconnected';

	let loadState = $state<LoadState>('loading');
	let loadErrorMessage = $state<string | null>(null);
	let shared = $state<SharedNoteResponse | null>(null);
	// REST content shown until the live room syncs.
	let restContent = $state('');
	let liveContent = $state('');

	let session = $state<CollabSession | null>(null);
	let connStatus = $state<ConnStatus>('connecting');
	let synced = $state(false);
	let peers = $state<PresencePeer[]>([]);
	let localClientId = -1;

	// Stable for this browser tab's lifetime (not persisted): random guest id
	// -> deterministic color, editable display name.
	const guestId = `guest-${crypto.randomUUID()}`;
	const guestColor = deriveUserColor(guestId);
	let guestName = $state(`Guest-${guestId.slice(-4).toUpperCase()}`);

	async function load(token: string): Promise<boolean> {
		loadState = 'loading';
		loadErrorMessage = null;
		try {
			const res = await apiGet<SharedNoteResponse>(`/api/shared/${encodeURIComponent(token)}`);
			shared = res;
			restContent = res.content;
			liveContent = res.content;
			loadState = 'ok';
			return true;
		} catch (err) {
			if (err instanceof ApiError && err.status === 404) {
				loadState = 'not-found';
				return false;
			}
			loadState = 'error';
			loadErrorMessage = err instanceof Error ? err.message : 'Failed to load shared note';
			return false;
		}
	}

	function refreshPeers(sess: CollabSession) {
		const list: PresencePeer[] = [];
		sess.awareness.getStates().forEach((state, clientId) => {
			const user = (state as { user?: { name?: string; color?: string } }).user;
			if (!user) return;
			list.push({
				clientId,
				name: user.name ?? 'Anonymous',
				color: user.color ?? 'var(--kv-dim)',
				self: clientId === localClientId
			});
		});
		list.sort((a, b) => (a.self === b.self ? a.clientId - b.clientId : a.self ? -1 : 1));
		peers = list;
	}

	// Lifecycle keyed on the token: resolve via REST, then join the collab
	// room. Re-runs (and tears down cleanly) if the token param ever changes.
	$effect(() => {
		const token = data.token;
		let cancelled = false;
		let sess: CollabSession | null = null;

		session = null;
		synced = false;
		connStatus = 'connecting';
		peers = [];
		localClientId = -1;

		const onStatus = (e: { status: ConnStatus }) => {
			connStatus = e.status;
		};
		const onSync = (isSynced: boolean) => {
			synced = isSynced;
		};

		(async () => {
			const ok = await load(token);
			if (cancelled || !ok) return;

			sess = createGuestCollabSession(token, {
				id: guestId,
				name: guestName,
				color: guestColor
			});
			localClientId = sess.awareness.clientID;
			const onAwareness = () => sess && refreshPeers(sess);

			sess.provider.on('status', onStatus);
			sess.provider.on('sync', onSync);
			sess.awareness.on('change', onAwareness);
			synced = sess.provider.synced;
			refreshPeers(sess);

			(sess as CollabSession & { _off?: () => void })._off = () => {
				sess?.provider.off('status', onStatus);
				sess?.provider.off('sync', onSync);
				sess?.awareness.off('change', onAwareness);
			};

			session = sess;
		})();

		return () => {
			cancelled = true;
			const s = sess as (CollabSession & { _off?: () => void }) | null;
			s?._off?.();
			s?.destroy();
			session = null;
		};
	});

	// Renaming updates the live awareness state so peers see the new name
	// without reconnecting.
	function onNameChange() {
		session?.awareness.setLocalStateField('user', { name: guestName, color: guestColor });
	}

	function onEditorChange(v: string) {
		liveContent = v;
	}

	const wordCount = $derived(
		liveContent.trim() === '' ? 0 : liveContent.trim().split(/\s+/).length
	);
	const lineCount = $derived(liveContent === '' ? 0 : liveContent.split('\n').length);

	const status = $derived.by(() => {
		if (connStatus === 'disconnected') {
			return { label: 'offline · reconnecting', tone: 'warn' as const, dot: 'var(--kv-orange)' };
		}
		if (synced) {
			return { label: 'synced', tone: 'ok' as const, dot: 'var(--kv-accent)' };
		}
		if (connStatus === 'connected') {
			return { label: 'syncing…', tone: 'ok' as const, dot: 'var(--kv-accent)' };
		}
		return { label: 'connecting…', tone: 'dim' as const, dot: 'var(--kv-dim)' };
	});
</script>

<svelte:head>
	<title>{shared?.title ?? 'Shared note'} · rust_note</title>
</svelte:head>

{#if loadState === 'loading'}
	<p class="center-message dim">Loading…</p>
{:else if loadState === 'not-found'}
	<p class="center-message warn">This link is invalid or has expired.</p>
{:else if loadState === 'error'}
	<p class="center-message danger">Error loading shared note: {loadErrorMessage}</p>
{:else if shared}
	<div class="shared-page kv-scanlines">
		<div class="shared-header">
			<div class="shared-title-group">
				<span class="shared-eyebrow">&gt; public share link</span>
				<h1 class="shared-title">{shared.title}</h1>
				<span class="shared-owner">Shared by {shared.owner_display_name}</span>
			</div>
			<div class="shared-header-right">
				<Chip color={shared.permission === 'edit' ? 'orange' : 'accent'} variant="outline">
					{shared.permission}
				</Chip>
				<CollabPresence {peers} />
			</div>
		</div>

		<div class="shared-identity">
			<label class="shared-identity-label" for="guest-name">You are</label>
			<input
				id="guest-name"
				class="shared-identity-input"
				type="text"
				bind:value={guestName}
				onblur={onNameChange}
				onkeydown={(e) => {
					if (e.key === 'Enter') (e.target as HTMLInputElement).blur();
				}}
				maxlength={40}
			/>
		</div>

		{#if session && synced}
			<CodeMirrorEditor
				collab={{
					ytext: session.ytext,
					awareness: session.awareness,
					undoManager: session.undoManager
				}}
				editable={shared.permission === 'edit'}
				onChange={onEditorChange}
			/>
		{:else}
			<div class="preview" class:offline={connStatus === 'disconnected'}>
				<pre>{restContent}</pre>
			</div>
		{/if}

		<div class="shared-footer">
			<span class="save-status" class:warn={status.tone === 'warn'}>
				<PulsingDot color={status.dot} />
				<span>{status.label}</span>
			</span>
			<span class="shared-stats">{wordCount} words · {lineCount} lines</span>
		</div>
	</div>
{/if}

<style>
	.center-message {
		font-family: var(--font-term);
		font-size: var(--type-body);
		text-align: center;
		margin-top: var(--space-10);
	}

	.center-message.dim {
		color: var(--kv-dim);
	}

	.center-message.warn {
		color: var(--kv-orange);
	}

	.center-message.danger {
		color: var(--kv-danger);
	}

	.shared-page {
		max-width: 60rem;
		margin: 0 auto;
		display: flex;
		flex-direction: column;
		background: var(--surface-card);
		border: 1px solid var(--border-default);
		border-radius: var(--radius-card);
	}

	.shared-header {
		display: flex;
		align-items: flex-start;
		justify-content: space-between;
		gap: var(--space-6);
		padding: var(--space-5) var(--space-6);
		background: var(--surface-card);
		border-bottom: 1px solid var(--border-default);
	}

	.shared-title-group {
		display: flex;
		flex-direction: column;
		gap: var(--space-2);
		min-width: 0;
	}

	.shared-eyebrow {
		font-family: var(--font-pixel);
		font-size: var(--type-label);
		letter-spacing: var(--tracking-pixel);
		text-transform: uppercase;
		color: var(--kv-orange);
	}

	.shared-title {
		margin: 0;
		font-family: var(--font-term);
		font-size: var(--type-body);
		font-weight: normal;
		color: var(--kv-ink);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.shared-owner {
		font-family: var(--font-term);
		font-size: var(--type-meta);
		color: var(--kv-dim);
	}

	.shared-header-right {
		display: flex;
		align-items: center;
		gap: var(--space-5);
		flex: 0 0 auto;
	}

	.shared-identity {
		display: flex;
		align-items: center;
		gap: var(--space-3);
		padding: var(--space-3) var(--space-6);
		border-bottom: 1px solid var(--border-default);
	}

	.shared-identity-label {
		font-family: var(--font-pixel);
		font-size: var(--type-label);
		letter-spacing: var(--tracking-pixel);
		text-transform: uppercase;
		color: var(--kv-dim);
	}

	.shared-identity-input {
		background: var(--surface-input);
		border: 1px solid var(--border-default);
		border-radius: var(--radius-control);
		color: var(--kv-ink);
		font-family: var(--font-term);
		font-size: var(--type-meta);
		padding: 4px 8px;
		max-width: 12rem;
	}

	.shared-identity-input:focus {
		outline: none;
		border-color: var(--kv-accent);
	}

	.preview {
		border-top: 1px solid var(--border-default);
		border-bottom: 1px solid var(--border-default);
		min-height: 60vh;
		max-height: 60vh;
		overflow: auto;
		background: var(--surface-input);
	}

	.preview pre {
		margin: 0;
		padding: var(--space-2) var(--space-4);
		font-family: var(--font-term);
		font-size: var(--type-body);
		line-height: var(--leading-term);
		color: var(--kv-dim);
		white-space: pre-wrap;
		word-break: break-word;
	}

	.preview.offline pre {
		color: var(--kv-faint);
	}

	.shared-footer {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: var(--space-6);
		padding: var(--space-4) var(--space-6);
		background: var(--surface-card);
		border-top: 1px solid var(--border-default);
	}

	.save-status {
		display: inline-flex;
		align-items: center;
		gap: var(--space-3);
		font-family: var(--font-term);
		font-size: var(--type-meta);
		color: var(--kv-dim);
	}

	.save-status.warn {
		color: var(--kv-orange);
	}

	.shared-stats {
		font-family: var(--font-term);
		font-size: var(--type-meta);
		color: var(--kv-faint);
		white-space: nowrap;
	}
</style>
