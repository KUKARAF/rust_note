<script lang="ts">
	import type { PageProps } from './$types';
	import { get } from 'svelte/store';
	import { apiGet, ApiError } from '$lib/api/client';
	import { goto, beforeNavigate } from '$app/navigation';
	import { resolve } from '$app/paths';
	import CodeMirrorEditor from '$lib/editor/CodeMirrorEditor.svelte';
	import CollabPresence, { type PresencePeer } from '$lib/editor/CollabPresence.svelte';
	import PulsingDot from '$lib/design/PulsingDot.svelte';
	import Button from '$lib/design/Button.svelte';
	import ShareDialog from '$lib/share/ShareDialog.svelte';
	import { auth } from '$lib/stores/auth';
	import {
		createCollabSession,
		deriveUserColor,
		type CollabSession,
		type CollabUser
	} from '$lib/editor/collabProvider';

	interface NoteMeta {
		id: string;
		title: string;
		owner_id: string;
		created_at: string;
		updated_at: string;
		version: string;
	}

	interface NoteResponse {
		meta: NoteMeta;
		content: string;
	}

	let { data }: PageProps = $props();

	type LoadState = 'loading' | 'ok' | 'not-found' | 'forbidden' | 'error';
	type ConnStatus = 'connecting' | 'connected' | 'disconnected';

	let loadState = $state<LoadState>('loading');
	let loadErrorMessage = $state<string | null>(null);
	let meta = $state<NoteMeta | null>(null);
	// The REST content is used only for fast first paint / a read-only preview
	// until the live CRDT room has synced. Once synced, the editor is the
	// authoritative view and this is no longer displayed.
	let restContent = $state('');
	// Live word/line counts come from the editor once it's mounted.
	let liveContent = $state('');

	// Collab session lifecycle.
	let session = $state<CollabSession | null>(null);
	let connStatus = $state<ConnStatus>('connecting');
	let synced = $state(false);
	let peers = $state<PresencePeer[]>([]);
	let localClientId = -1;

	let shareDialogOpen = $state(false);

	// `data.path` is the SvelteKit catch-all param (already URL-decoded). Note
	// ids from the filesystem can contain '#', '?', '%', '&', spaces, etc., so
	// re-encode each segment before building a REST URL. (The collab room name
	// is encoded the same way inside collabProvider.)
	function encodeNotePath(path: string): string {
		return path.split('/').map(encodeURIComponent).join('/');
	}

	async function load(path: string): Promise<boolean> {
		loadState = 'loading';
		loadErrorMessage = null;
		try {
			const note = await apiGet<NoteResponse>(`/api/notes/${encodeNotePath(path)}`);
			meta = note.meta;
			restContent = note.content;
			liveContent = note.content;
			loadState = 'ok';
			return true;
		} catch (err) {
			if (err instanceof ApiError && err.status === 401) {
				await goto(resolve('/login'));
				return false;
			}
			if (err instanceof ApiError && err.status === 404) {
				loadState = 'not-found';
				return false;
			}
			if (err instanceof ApiError && err.status === 403) {
				loadState = 'forbidden';
				return false;
			}
			loadState = 'error';
			loadErrorMessage = err instanceof Error ? err.message : 'Failed to load note';
			return false;
		}
	}

	function currentUser(): CollabUser {
		const u = get(auth).user;
		if (u) {
			const name = u.display_name ?? u.email ?? 'You';
			return { id: u.id, name, color: deriveUserColor(u.id) };
		}
		// No authed user (e.g. a future shared-link guest): stable-enough random
		// identity so the caret still has a consistent color for this session.
		const id = `guest-${Math.random().toString(36).slice(2, 10)}`;
		return { id, name: 'You', color: deriveUserColor(id) };
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
		// Stable order (self first, then by clientId) so chips don't jump around.
		list.sort((a, b) => (a.self === b.self ? a.clientId - b.clientId : a.self ? -1 : 1));
		peers = list;
	}

	// Single lifecycle effect keyed on the note path: load via REST, then join
	// the collab room. Re-runs (and tears down cleanly) when navigating between
	// notes without a full remount.
	$effect(() => {
		const path = data.path;
		let cancelled = false;
		let sess: CollabSession | null = null;

		// Reset per-note UI state.
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
			const ok = await load(path);
			if (cancelled || !ok) return;

			sess = createCollabSession(path, currentUser());
			localClientId = sess.awareness.clientID;
			const onAwareness = () => sess && refreshPeers(sess);

			sess.provider.on('status', onStatus);
			sess.provider.on('sync', onSync);
			sess.awareness.on('change', onAwareness);
			// Reflect any state already present (e.g. our own local state).
			synced = sess.provider.synced;
			refreshPeers(sess);

			// Stash de-registration on the session for cleanup below.
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

	// Guard against navigating away mid-connect, before edits have synced. Once
	// the room is synced, edits persist automatically (server-side debounced
	// commit), so no unsaved-changes prompt is needed for the common case.
	beforeNavigate((navigation) => {
		if (loadState !== 'ok') return;
		if (synced) return; // live edits are already syncing/persisting
		if (liveContent === restContent) return; // nothing typed yet
		if (!confirm('This note is still connecting — leave before your changes sync?')) {
			navigation.cancel();
		}
	});

	function onEditorChange(v: string) {
		liveContent = v;
	}

	// Ctrl+S is a no-op here: persistence is automatic and firing a REST PUT
	// would fight the live room. We keep the binding so the browser's Save
	// dialog never pops, and nudge the user that saving is automatic.
	let showSaveHint = $state(false);
	let saveHintTimer: ReturnType<typeof setTimeout> | undefined;
	function onSave() {
		showSaveHint = true;
		clearTimeout(saveHintTimer);
		saveHintTimer = setTimeout(() => (showSaveHint = false), 2200);
	}

	function onWindowKeydown(event: KeyboardEvent) {
		if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === 's') {
			event.preventDefault();
			onSave();
		}
	}

	const wordCount = $derived(liveContent.trim() === '' ? 0 : liveContent.trim().split(/\s+/).length);
	const lineCount = $derived(liveContent === '' ? 0 : liveContent.split('\n').length);

	// Split `data.path` into a dimmed "folder" portion and a bright filename.
	const pathParts = $derived.by(() => {
		const idx = data.path.lastIndexOf('/');
		if (idx === -1) return { folder: '', name: data.path };
		return { folder: data.path.slice(0, idx + 1), name: data.path.slice(idx + 1) };
	});

	// Sync-status indicator model.
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

<svelte:window onkeydown={onWindowKeydown} />

<svelte:head>
	<title>{meta?.title ?? data.path} · rust_note</title>
</svelte:head>

{#if loadState === 'loading'}
	<p class="center-message dim">Loading…</p>
{:else if loadState === 'not-found'}
	<p class="center-message dim">Note not found: <code>{data.path}</code></p>
{:else if loadState === 'forbidden'}
	<p class="center-message warn">You don't have access to this note.</p>
{:else if loadState === 'error'}
	<p class="center-message danger">Error loading note: {loadErrorMessage}</p>
{:else if meta}
	<div class="editor-page kv-scanlines">
		<div class="editor-header">
			<h1 class="editor-path">
				{#if pathParts.folder}<span class="folder">{pathParts.folder}</span>{/if}<span
					class="filename">{pathParts.name}</span
				>
			</h1>
			<div class="editor-header-actions">
				<CollabPresence {peers} />
				<Button variant="outline" size="sm" onclick={() => (shareDialogOpen = true)}>
					Share
				</Button>
			</div>
		</div>

		{#if shareDialogOpen}
			<ShareDialog
				noteId={data.path}
				noteTitle={meta.title}
				onClose={() => (shareDialogOpen = false)}
			/>
		{/if}

		{#if session && synced}
			<!-- Live collaborative editor: seeded from the synced CRDT. -->
			<CodeMirrorEditor
				collab={{
					ytext: session.ytext,
					awareness: session.awareness,
					undoManager: session.undoManager
				}}
				onChange={onEditorChange}
				{onSave}
			/>
		{:else}
			<!-- Read-only preview from REST until the room syncs (or if the WS is
			     unreachable — graceful degradation: content stays readable). -->
			<div class="preview" class:offline={connStatus === 'disconnected'}>
				<pre>{restContent}</pre>
			</div>
		{/if}

		<div class="editor-footer">
			<span class="save-status" class:warn={status.tone === 'warn'}>
				<PulsingDot color={status.dot} />
				<span>{status.label}</span>
				{#if showSaveHint}
					<span class="save-hint">— saves automatically</span>
				{/if}
			</span>
			<span class="editor-stats">{wordCount} words · {lineCount} lines</span>
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

	.editor-page {
		max-width: 60rem;
		margin: 0 auto;
		display: flex;
		flex-direction: column;
		background: var(--surface-card);
		border: 1px solid var(--border-default);
		border-radius: var(--radius-card);
	}

	.editor-header {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: var(--space-6);
		padding: var(--space-5) var(--space-6);
		background: var(--surface-card);
		border-bottom: 1px solid var(--border-default);
	}

	.editor-path {
		margin: 0;
		font-family: var(--font-term);
		font-size: var(--type-body);
		font-weight: normal;
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.editor-header-actions {
		display: flex;
		align-items: center;
		gap: var(--space-5);
		flex: 0 0 auto;
	}

	.folder {
		color: var(--kv-dim);
	}

	.filename {
		color: var(--kv-ink);
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

	.editor-footer {
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

	.save-hint {
		color: var(--kv-faint);
	}

	.editor-stats {
		font-family: var(--font-term);
		font-size: var(--type-meta);
		color: var(--kv-faint);
		white-space: nowrap;
	}
</style>
