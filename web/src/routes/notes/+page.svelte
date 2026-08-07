<script lang="ts">
	// Note list view. This route is static-shaped (no dynamic params), so —
	// consistent with adapter-static/ssr=false — data is fetched client-side
	// on mount rather than via a `+page.ts` load function (the dynamic
	// `[...path]` route uses `+page.ts` only to pass through the route param,
	// not to fetch data, since that fetch also needs to happen client-side).
	import { onMount, tick } from 'svelte';
	import { goto } from '$app/navigation';
	import { resolve } from '$app/paths';
	import { apiGet, apiPost, ApiError } from '$lib/api/client';
	import { cacheNotesList, readNotesListCache } from '$lib/stores/offline';
	import { encodeNotePath } from '$lib/notes/path';
	import Card from '$lib/design/Card.svelte';
	import Button from '$lib/design/Button.svelte';
	import Input from '$lib/design/Input.svelte';
	import SectionTitle from '$lib/design/SectionTitle.svelte';

	interface NoteMeta {
		id: string;
		title: string;
		owner_id: string;
		created_at: string;
		updated_at: string;
	}

	let notes = $state<NoteMeta[]>([]);
	let loading = $state(true);
	let loadError = $state<string | null>(null);
	// Timestamp of the cached list currently shown because the server is
	// unreachable; null when the list is live.
	let offlineCachedAt = $state<number | null>(null);
	let filter = $state('');
	let selectedIndex = $state(0);
	let creating = $state(false);
	let createError = $state<string | null>(null);

	let filterInput: HTMLInputElement | undefined = $state();
	let listEl: HTMLUListElement | undefined = $state();

	const filtered = $derived(
		filter.trim() === ''
			? notes
			: notes.filter((n) => {
					const q = filter.toLowerCase();
					return n.title.toLowerCase().includes(q) || n.id.toLowerCase().includes(q);
				})
	);

	async function loadNotes() {
		loading = true;
		loadError = null;
		try {
			notes = await apiGet<NoteMeta[]>('/api/notes');
			// Cache for offline fallback (and mark the shown list as live).
			cacheNotesList(notes);
			offlineCachedAt = null;
		} catch (err) {
			if (err instanceof ApiError && err.status === 401) {
				await goto(resolve('/login'));
				return;
			}
			if (err instanceof ApiError && err.status === 0) {
				// Offline: fall back to the last successfully fetched list, with
				// a banner noting its age. No cache -> a dedicated offline error.
				const cached = readNotesListCache();
				if (cached) {
					notes = cached.notes;
					offlineCachedAt = cached.at;
				} else {
					loadError = "You're offline and the note list isn't cached on this device.";
				}
				return;
			}
			loadError = err instanceof Error ? err.message : 'Failed to load notes';
		} finally {
			loading = false;
		}
	}

	onMount(() => {
		void loadNotes();

		// Refresh whenever the app returns to the foreground (tab refocused,
		// Android webview resumed), so a list opened offline goes live again as
		// soon as connectivity is back — without a manual reload.
		const onVisibilityChange = () => {
			if (document.visibilityState === 'visible') void loadNotes();
		};
		const onFocus = () => void loadNotes();
		document.addEventListener('visibilitychange', onVisibilityChange);
		window.addEventListener('focus', onFocus);
		return () => {
			document.removeEventListener('visibilitychange', onVisibilityChange);
			window.removeEventListener('focus', onFocus);
		};
	});

	/** Coarse "how stale is the cached list" hint for the offline banner. */
	function relativeAge(at: number): string {
		const minutes = Math.round((Date.now() - at) / 60_000);
		if (minutes < 1) return 'just now';
		if (minutes < 60) return `${minutes}m ago`;
		const hours = Math.round(minutes / 60);
		if (hours < 24) return `${hours}h ago`;
		return `${Math.round(hours / 24)}d ago`;
	}

	// Keep the selection in range whenever the filtered list changes.
	$effect(() => {
		if (selectedIndex >= filtered.length) {
			selectedIndex = Math.max(0, filtered.length - 1);
		}
	});

	function openSelected() {
		const note = filtered[selectedIndex];
		if (note) void goto(resolve(`/notes/${encodeNotePath(note.id)}`));
	}

	function moveSelection(delta: number) {
		if (filtered.length === 0) return;
		selectedIndex = Math.min(Math.max(selectedIndex + delta, 0), filtered.length - 1);
		scrollSelectedIntoView();
	}

	async function scrollSelectedIntoView() {
		await tick();
		const el = listEl?.querySelector<HTMLElement>('[data-selected="true"]');
		el?.scrollIntoView({ block: 'nearest' });
	}

	function onKeydown(event: KeyboardEvent) {
		// Modifier combos belong to global shortcuts (Ctrl/Cmd+K opens the
		// command palette in the layout) — never treat them as list navigation.
		if (event.ctrlKey || event.metaKey || event.altKey) return;

		const target = event.target as HTMLElement | null;
		const isFilterFocused = target === filterInput;

		if (isFilterFocused) {
			if (event.key === 'Escape') {
				filterInput?.blur();
			} else if (event.key === 'Enter') {
				event.preventDefault();
				openSelected();
			}
			return;
		}

		switch (event.key) {
			case 'j':
			case 'ArrowDown':
				event.preventDefault();
				moveSelection(1);
				break;
			case 'k':
			case 'ArrowUp':
				event.preventDefault();
				moveSelection(-1);
				break;
			case 'Enter':
				event.preventDefault();
				openSelected();
				break;
			case '/':
				event.preventDefault();
				filterInput?.focus();
				break;
		}
	}

	async function createNote() {
		const title = window.prompt('Title for the new note:');
		if (!title || !title.trim()) return;
		creating = true;
		createError = null;
		try {
			const meta = await apiPost<NoteMeta>('/api/notes', { id_or_title: title.trim() });
			await goto(resolve(`/notes/${encodeNotePath(meta.id)}`));
		} catch (err) {
			if (err instanceof ApiError && err.status === 401) {
				await goto(resolve('/login'));
				return;
			}
			if (err instanceof ApiError && err.status === 0) {
				// Creation needs the server (it allocates the id and the git
				// commit) — there's no offline queue for it, by design.
				createError = "You're offline — creating notes requires a connection.";
				return;
			}
			const message = err instanceof Error ? err.message : 'Failed to create note';
			window.alert(message);
		} finally {
			creating = false;
		}
	}
</script>

<svelte:window onkeydown={onKeydown} />

<div class="notes-page">
	<Card class="notes-card">
		<div class="notes-header">
			<SectionTitle>Vault</SectionTitle>
			<Button variant="primary" size="sm" onclick={createNote} disabled={creating}>
				{creating ? 'Creating…' : '+ New note'}
			</Button>
		</div>

		{#if createError}
			<p class="status-line error">{createError}</p>
		{/if}

		{#if offlineCachedAt !== null}
			<p class="offline-banner">
				offline — showing cached list ({relativeAge(offlineCachedAt)})
			</p>
		{/if}

		<div class="filter-row">
			<Input type="text" placeholder="Filter notes…" bind:value={filter} bind:el={filterInput}>
				{#snippet prefix()}&gt;{/snippet}
				{#snippet suffix()}/{/snippet}
			</Input>
		</div>

		{#if loading}
			<p class="status-line">Loading notes…</p>
		{:else if loadError}
			<p class="status-line error">Error loading notes: {loadError}</p>
		{:else if notes.length === 0}
			<p class="status-line">No notes yet. Create your first one above.</p>
		{:else if filtered.length === 0}
			<p class="status-line">No notes match "{filter}".</p>
		{:else}
			<ul class="notes-list" bind:this={listEl}>
				{#each filtered as note, index (note.id)}
					<li>
						<a
							href={resolve(`/notes/${encodeNotePath(note.id)}`)}
							data-selected={index === selectedIndex}
							class:selected={index === selectedIndex}
							onmouseenter={() => (selectedIndex = index)}
						>
							<span class="note-title" title={note.title}
								>{note.title.trim() ? note.title : '(untitled)'}</span
							>
							<span class="note-id" title={note.id}>{note.id}</span>
						</a>
					</li>
				{/each}
			</ul>

			<p class="notes-count">— {filtered.length} note{filtered.length === 1 ? '' : 's'} —</p>
		{/if}

		<p class="hint">
			Keyboard: <kbd>j</kbd>/<kbd>k</kbd> or arrows to move, <kbd>Enter</kbd> to open, <kbd>/</kbd> to
			filter.
		</p>
	</Card>
</div>

<style>
	.notes-page {
		max-width: 42rem;
		margin: 0 auto;
	}

	:global(.notes-card) {
		display: flex;
		flex-direction: column;
	}

	.notes-header {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: var(--space-6);
		margin-bottom: var(--space-6);
	}

	.filter-row {
		margin-bottom: var(--space-6);
	}

	.notes-list {
		list-style: none;
		margin: 0;
		padding: 0;
		border-top: 1px solid var(--border-default);
	}

	.notes-list a {
		display: flex;
		justify-content: space-between;
		align-items: baseline;
		gap: var(--space-6);
		padding: var(--space-4) var(--space-3);
		text-decoration: none;
		color: inherit;
		min-width: 0;
		border-left: 2px solid transparent;
		border-bottom: 1px solid var(--border-default);
		font-family: var(--font-term);
		font-size: var(--type-data);
		transition:
			background 120ms linear,
			border-color 120ms linear;
	}

	.notes-list a:hover,
	.notes-list a.selected {
		background: rgba(121, 242, 121, 0.07);
		border-left: 2px solid var(--kv-accent);
	}

	.note-title {
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		min-width: 0;
		flex: 1 1 auto;
		color: var(--kv-ink);
	}

	.note-id {
		color: var(--kv-dim);
		font-size: var(--type-meta);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		flex: 0 1 auto;
		min-width: 0;
	}

	.status-line {
		font-family: var(--font-term);
		font-size: var(--type-body);
		color: var(--kv-dim);
	}

	.status-line.error {
		color: var(--kv-danger);
	}

	.offline-banner {
		font-family: var(--font-term);
		font-size: var(--type-meta);
		color: var(--kv-faint);
		margin: 0 0 var(--space-4);
	}

	.notes-count {
		text-align: center;
		font-family: var(--font-term);
		font-size: var(--type-meta);
		color: var(--kv-faint);
		margin: var(--space-5) 0 0;
	}

	.hint {
		margin-top: var(--space-7);
		font-family: var(--font-term);
		font-size: var(--type-meta);
		color: var(--kv-dim);
	}

	kbd {
		border: 1px solid var(--border-default);
		border-bottom-width: 2px;
		border-radius: var(--radius-control);
		padding: 0 5px;
		font-family: var(--font-pixel);
		font-size: 8px;
		color: var(--kv-ink);
	}
</style>
