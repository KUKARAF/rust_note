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
	import TrackValueDialog from '$lib/notes/TrackValueDialog.svelte';
	import ExcalidrawView from '$lib/notes/ExcalidrawView.svelte';
	import { isExcalidrawNote, parseExcalidrawScene } from '$lib/notes/excalidraw';
	import { DAILY_NOTE_RE } from '$lib/notes/daily';
	import { auth, flagLoginRequired } from '$lib/stores/auth';
	import { cacheNoteMeta, hasLocalCopy, markNoteSynced, readNoteMeta } from '$lib/stores/offline';
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

	// 'offline-uncached' is the dead end: the server is unreachable AND this
	// note has never synced on this device, so there is nothing to show.
	type LoadState = 'loading' | 'ok' | 'not-found' | 'forbidden' | 'error' | 'offline-uncached';
	type ConnStatus = 'connecting' | 'connected' | 'disconnected';
	// What the REST load produced; 'offline' (network error) is not necessarily
	// fatal — the lifecycle effect below may still open a local copy.
	type LoadOutcome = 'ok' | 'offline' | 'failed';

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
	// Offline-first: true once y-indexeddb has loaded the locally persisted
	// doc, which is enough to mount the editor when the server is unreachable.
	let localReady = $state(false);
	// True when this note was opened from its local copy (REST load failed with
	// a network error) — drives the "edits saved locally" status chip.
	let offlineOpen = $state(false);
	let peers = $state<PresencePeer[]>([]);
	let localClientId = -1;

	let shareDialogOpen = $state(false);
	let trackDialogOpen = $state(false);
	// Metric tracking only applies to daily notes (diary/YYYY-MM-DD).
	const isDailyNote = $derived(DAILY_NOTE_RE.test(data.path));

	// Excalidraw drawings render as SVG by default, with a raw-text toggle
	// (the wrapper markdown stays editable — that's the editing story until
	// a full drawing editor lands). `drawingText` mirrors the live doc for
	// the renderer since CodeMirror isn't mounted in drawing view.
	const isDrawing = $derived(isExcalidrawNote(data.path));
	let drawingAsText = $state(false);
	let drawingText = $state('');
	// Full drawing editor (lazy-loaded React overlay). While it's open the
	// awareness state carries a `drawingEditing` flag so other clients can
	// warn that concurrent saves are last-save-wins (see peerEditingDrawing).
	let drawingEditorOpen = $state(false);
	let peerEditingDrawing = $state(false);

	// Stale-CRDT repair: collab rooms seed from the server's persisted CRDT
	// state, which for drawings can predate the server-side migration to bare
	// scene JSON (disk is authoritative for drawings). When the live doc has
	// no parseable scene but the REST content (read from disk) does, offer a
	// one-click whole-doc replace with the disk content.
	const drawingRepairAvailable = $derived(
		isDrawing &&
			synced &&
			parseExcalidrawScene(drawingText) === null &&
			parseExcalidrawScene(restContent) !== null
	);

	function repairDrawing() {
		const s = session;
		if (!s) return;
		const target = restContent;
		const doc = s.ytext.doc;
		// Single transaction: peers see one atomic replace, not a delete+insert.
		const apply = () => {
			s.ytext.delete(0, s.ytext.length);
			s.ytext.insert(0, target);
		};
		if (doc) {
			doc.transact(apply);
		} else {
			apply();
		}
	}

	// `data.path` is the SvelteKit catch-all param (already URL-decoded). Note
	// ids from the filesystem can contain '#', '?', '%', '&', spaces, etc., so
	// re-encode each segment before building a REST URL. (The collab room name
	// is encoded the same way inside collabProvider.)
	function encodeNotePath(path: string): string {
		return path.split('/').map(encodeURIComponent).join('/');
	}

	async function load(path: string): Promise<LoadOutcome> {
		loadState = 'loading';
		loadErrorMessage = null;
		try {
			const note = await apiGet<NoteResponse>(`/api/notes/${encodeNotePath(path)}`);
			meta = note.meta;
			restContent = note.content;
			liveContent = note.content;
			// Seed the drawing renderer immediately from REST; the collab
			// observer takes over once the session is live.
			drawingText = note.content;
			// Remember the metadata so the note header can still render if this
			// note is later opened offline (the CRDT copy only has content).
			cacheNoteMeta(path, note.meta);
			loadState = 'ok';
			return 'ok';
		} catch (err) {
			if (err instanceof ApiError && err.status === 401) {
				flagLoginRequired();
				await goto(resolve('/login'));
				return 'failed';
			}
			if (err instanceof ApiError && err.status === 404) {
				loadState = 'not-found';
				return 'failed';
			}
			if (err instanceof ApiError && err.status === 403) {
				loadState = 'forbidden';
				return 'failed';
			}
			if (err instanceof ApiError && err.status === 0) {
				// Server unreachable — leave loadState alone; the caller decides
				// whether a local copy makes this note openable anyway.
				return 'offline';
			}
			loadState = 'error';
			loadErrorMessage = err instanceof Error ? err.message : 'Failed to load note';
			return 'failed';
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
		let otherDrawing = false;
		sess.awareness.getStates().forEach((state, clientId) => {
			// Soft-lock signal: some OTHER client has the drawing editor open.
			if (clientId !== localClientId) {
				const s = state as { drawingEditing?: unknown };
				if (s.drawingEditing) otherDrawing = true;
			}
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
		peerEditingDrawing = otherDrawing;
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
		localReady = false;
		offlineOpen = false;
		connStatus = 'connecting';
		peers = [];
		localClientId = -1;
		drawingAsText = false;
		drawingText = '';
		drawingEditorOpen = false;
		peerEditingDrawing = false;

		const onStatus = (e: { status: ConnStatus }) => {
			connStatus = e.status;
		};
		const onSync = (isSynced: boolean) => {
			synced = isSynced;
			// A full sync means y-indexeddb now holds a complete copy of the
			// doc, so this note becomes openable offline on this device.
			if (isSynced) markNoteSynced(path);
		};

		(async () => {
			const outcome = await load(path);
			if (cancelled || outcome === 'failed') return;
			if (outcome === 'offline') {
				if (!hasLocalCopy(path)) {
					// Offline and never synced here: dead end (rendered below).
					loadState = 'offline-uncached';
					return;
				}
				// Offline, but the doc has a local CRDT copy: open from it. Meta
				// comes from the per-note cache (a stub if that's missing — it
				// only feeds the title/header), and the collab session is still
				// created: y-indexeddb loads the doc while y-websocket keeps
				// retrying in the background and merges once we're back online.
				meta = readNoteMeta(path) ?? {
					id: path,
					title: path.split('/').pop() ?? path,
					owner_id: '',
					created_at: '',
					updated_at: '',
					version: ''
				};
				restContent = '';
				liveContent = '';
				offlineOpen = true;
				loadState = 'ok';
			}

			sess = createCollabSession(path, currentUser());
			if (outcome === 'offline') {
				// Mount the editor as soon as the local copy is in, without
				// waiting for a websocket sync that may never come.
				void sess.whenLocalLoaded.then(() => {
					if (!cancelled) localReady = true;
				});
			}
			localClientId = sess.awareness.clientID;
			const onAwareness = () => sess && refreshPeers(sess);

			sess.provider.on('status', onStatus);
			sess.provider.on('sync', onSync);
			sess.awareness.on('change', onAwareness);
			// Reflect any state already present (e.g. our own local state).
			synced = sess.provider.synced;
			refreshPeers(sess);

			// Drawing view: mirror the live doc text for the SVG renderer
			// (CodeMirror isn't mounted, so onEditorChange never fires). The
			// observer dies with doc.destroy() in the cleanup below.
			if (isDrawing) {
				const s = sess;
				const updateDrawingText = () => {
					drawingText = s.ytext.toString();
				};
				s.doc.on('update', updateDrawingText);
				updateDrawingText();
			}

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

	// Broadcast the drawing-editor soft-lock over awareness while the overlay
	// is open, so other clients on this note can show the last-save-wins
	// warning. Cleared on close/note-switch; session.destroy() dropping the
	// whole local awareness state is the backstop for hard teardowns.
	$effect(() => {
		const s = session;
		if (!s) return;
		if (drawingEditorOpen) {
			s.awareness.setLocalStateField('drawingEditing', true);
			return () => {
				s.awareness.setLocalStateField('drawingEditing', null);
			};
		}
		s.awareness.setLocalStateField('drawingEditing', null);
	});

	// Guard against navigating away mid-connect, before edits have synced. Once
	// the room is synced, edits persist automatically (server-side debounced
	// commit), so no unsaved-changes prompt is needed for the common case.
	beforeNavigate((navigation) => {
		if (loadState !== 'ok') return;
		// Synced edits persist server-side; offline edits persist in IndexedDB
		// and merge on reconnect — either way, leaving loses nothing.
		if (synced || localReady) return;
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
		// The drawing overlay owns its keys (Ctrl+S saves the scene there); it
		// stops propagation itself, but never race it for the save shortcut.
		if (drawingEditorOpen) return;
		if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === 's') {
			event.preventDefault();
			onSave();
		}
	}

	const wordCount = $derived(
		liveContent.trim() === '' ? 0 : liveContent.trim().split(/\s+/).length
	);
	const lineCount = $derived(liveContent === '' ? 0 : liveContent.split('\n').length);

	// Split `data.path` into a dimmed "folder" portion and a bright filename.
	const pathParts = $derived.by(() => {
		const idx = data.path.lastIndexOf('/');
		if (idx === -1) return { folder: '', name: data.path };
		return { folder: data.path.slice(0, idx + 1), name: data.path.slice(idx + 1) };
	});

	// Sync-status indicator model.
	const status = $derived.by(() => {
		if (offlineOpen && connStatus !== 'connected') {
			// Opened from the local copy and still offline: edits land in
			// IndexedDB and sync automatically once the server is reachable.
			return {
				label: 'offline — edits saved locally',
				tone: 'warn' as const,
				dot: 'var(--kv-orange)'
			};
		}
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
{:else if loadState === 'offline-uncached'}
	<p class="center-message warn">You're offline and this note isn't cached on this device.</p>
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
				{#if isDailyNote && session && (synced || localReady)}
					<Button variant="outline" size="sm" onclick={() => (trackDialogOpen = true)}>Track</Button
					>
				{/if}
				{#if isDrawing}
					<Button
						variant="outline"
						size="sm"
						disabled={!session || !(synced || localReady)}
						onclick={() => (drawingEditorOpen = true)}
					>
						Edit drawing
					</Button>
					<Button variant="outline" size="sm" onclick={() => (drawingAsText = !drawingAsText)}>
						{drawingAsText ? 'View drawing' : 'Edit text'}
					</Button>
				{/if}
				<Button variant="outline" size="sm" onclick={() => (shareDialogOpen = true)}>Share</Button>
			</div>
		</div>

		{#if shareDialogOpen}
			<ShareDialog
				noteId={data.path}
				noteTitle={meta.title}
				onClose={() => (shareDialogOpen = false)}
			/>
		{/if}

		{#if trackDialogOpen && session}
			<TrackValueDialog ytext={session.ytext} onclose={() => (trackDialogOpen = false)} />
		{/if}

		{#if drawingEditorOpen && session}
			<!-- Dynamic import keeps the React/Excalidraw stack in a lazy chunk
			     loaded only when someone actually opens the drawing editor. -->
			{#await import('$lib/notes/ExcalidrawEditor.svelte') then { default: ExcalidrawEditor }}
				<ExcalidrawEditor ytext={session.ytext} onclose={() => (drawingEditorOpen = false)} />
			{/await}
		{/if}

		{#if isDrawing && peerEditingDrawing}
			<p class="drawing-banner">Someone else is editing this drawing — last save wins.</p>
		{/if}

		{#if drawingRepairAvailable}
			<div class="drawing-banner repair">
				<span
					>This drawing's live copy can't be parsed, but the file on disk can — the collab state is
					stale.</span
				>
				<Button variant="outline" size="sm" onclick={repairDrawing}>Repair drawing</Button>
			</div>
		{/if}

		{#if isDrawing && !drawingAsText}
			<!-- Excalidraw drawing: rendered read-only as SVG (edits still sync
			     live — the renderer observes the collab doc). Falls back to the
			     text editor automatically when the drawing can't be parsed. -->
			<ExcalidrawView content={drawingText} onunrenderable={() => (drawingAsText = true)} />
		{:else if session && (synced || localReady)}
			<!-- Live collaborative editor: seeded from the synced CRDT (or the
			     local IndexedDB copy when opened offline). -->
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

	.drawing-banner {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: var(--space-5);
		margin: 0;
		padding: var(--space-3) var(--space-6);
		font-family: var(--font-term);
		font-size: var(--type-meta);
		color: var(--kv-orange);
		border-bottom: 1px solid var(--border-default);
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
