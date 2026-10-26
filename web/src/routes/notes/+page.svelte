<script lang="ts">
	import { onMount, tick } from 'svelte';
	import { goto } from '$app/navigation';
	import { resolve } from '$app/paths';
	import { page } from '$app/state';
	import { apiGet, apiPost, ApiError } from '$lib/api/client';
	import { cacheNotesList, readNotesListCache } from '$lib/stores/offline';
	import { auth, flagLoginRequired } from '$lib/stores/auth';
	import { IS_APP } from '$lib/api/deviceToken';
	import { encodeNotePath } from '$lib/notes/path';
	import { getSubFolders, getDirectNotes, getBreadcrumbs, slugifySegment } from '$lib/notes/folders';
	import {
		buildActions,
		filterItems,
		notesSearchFocuser,
		type NoteMeta,
		type PaletteItem
	} from '$lib/commandPalette/items';
	import Card from '$lib/design/Card.svelte';
	import Button from '$lib/design/Button.svelte';
	import Input from '$lib/design/Input.svelte';
	import Chip from '$lib/design/Chip.svelte';
	import SectionTitle from '$lib/design/SectionTitle.svelte';
	import Breadcrumb from '$lib/design/Breadcrumb.svelte';

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
	let searchScope = $state<'folder' | 'all'>('folder');

	let filterInput: HTMLInputElement | undefined = $state();
	let listEl: HTMLUListElement | undefined = $state();

	// --- Folder state (derived from URL ?f= param) ---

	const currentFolder = $derived(page.url.searchParams.get('f') ?? '');
	const breadcrumbs = $derived(getBreadcrumbs(currentFolder));
	const folderFolders = $derived(getSubFolders(notes, currentFolder));
	const folderNotes = $derived(getDirectNotes(notes, currentFolder));

	// Reset scope to 'folder' whenever the search filter is cleared.
	$effect(() => {
		if (!filter) searchScope = 'folder';
	});

	// Reset selection index when the active folder changes.
	$effect(() => {
		// eslint-disable-next-line @typescript-eslint/no-unused-expressions
		currentFolder;
		selectedIndex = 0;
	});

	function setFolder(path: string) {
		filter = '';
		selectedIndex = 0;
		void goto(resolve('/notes') + (path ? '?f=' + encodeURIComponent(path) : ''));
	}

	function leaveFolder() {
		const parts = currentFolder.split('/');
		parts.pop();
		setFolder(parts.join('/'));
	}

	function onNoteDeleted(id: string) {
		notes = notes.filter((n) => n.id !== id);
		cacheNotesList(notes);
	}

	// Unified item list for keyboard navigation. Behaviour differs by context:
	// - Root, no filter    → actions + all notes (existing behaviour)
	// - Root, with filter  → matching actions + notes (existing behaviour)
	// - Folder, no filter  → sub-folder rows + direct-child notes
	// - Folder, with filter → search scoped to folder or all notes
	const items = $derived<PaletteItem[]>(
		(() => {
			if (currentFolder === '') {
				return filterItems(
					filter,
					buildActions({ user: $auth.user }),
					notes,
					Infinity,
					onNoteDeleted
				);
			}
			if (filter === '') {
				const folderRows: PaletteItem[] = folderFolders.map((f) => ({
					id: `folder:${f}`,
					label: (f.split('/').pop() ?? f) + '/',
					hint: undefined,
					group: 'folder' as const,
					run: () => setFolder(f)
				}));
				const noteRows: PaletteItem[] = folderNotes.map((n) => ({
					id: `note:${n.id}`,
					label: n.title,
					hint: n.id,
					group: 'note' as const,
					run: () => goto(resolve(`/notes/${encodeNotePath(n.id)}`))
				}));
				return [...folderRows, ...noteRows];
			}
			// Searching inside a folder: no actions (they're vault-global).
			const searchNotes = searchScope === 'folder' ? folderNotes : notes;
			return filterItems(filter, [], searchNotes, Infinity, onNoteDeleted);
		})()
	);

	const noteCount = $derived(items.filter((i) => i.group === 'note').length);

	async function loadNotes() {
		loading = true;
		loadError = null;
		try {
			notes = await apiGet<NoteMeta[]>('/api/notes');
			cacheNotesList(notes);
			offlineCachedAt = null;
		} catch (err) {
			if (err instanceof ApiError && err.status === 401) {
				flagLoginRequired();
				await goto(resolve('/login'));
				return;
			}
			if (err instanceof ApiError && err.status === 0) {
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

		notesSearchFocuser.set(() => filterInput?.focus());

		const onVisibilityChange = () => {
			if (document.visibilityState === 'visible') void loadNotes();
		};
		const onFocus = () => void loadNotes();
		document.addEventListener('visibilitychange', onVisibilityChange);
		window.addEventListener('focus', onFocus);
		return () => {
			document.removeEventListener('visibilitychange', onVisibilityChange);
			window.removeEventListener('focus', onFocus);
			notesSearchFocuser.set(null);
		};
	});

	function relativeAge(at: number): string {
		const minutes = Math.round((Date.now() - at) / 60_000);
		if (minutes < 1) return 'just now';
		if (minutes < 60) return `${minutes}m ago`;
		const hours = Math.round(minutes / 60);
		if (hours < 24) return `${hours}h ago`;
		return `${Math.round(hours / 24)}d ago`;
	}

	$effect(() => {
		if (selectedIndex >= items.length) {
			selectedIndex = Math.max(0, items.length - 1);
		}
	});

	function runSelected() {
		const item = items[selectedIndex];
		if (item) void item.run();
	}

	function moveSelection(delta: number) {
		if (items.length === 0) return;
		selectedIndex = Math.min(Math.max(selectedIndex + delta, 0), items.length - 1);
		scrollSelectedIntoView();
	}

	async function scrollSelectedIntoView() {
		await tick();
		const el = listEl?.querySelector<HTMLElement>('[data-selected="true"]');
		el?.scrollIntoView({ block: 'nearest' });
	}

	function onKeydown(event: KeyboardEvent) {
		if (event.ctrlKey || event.metaKey || event.altKey) return;

		const target = event.target as HTMLElement | null;
		const isFilterFocused = target === filterInput;

		if (isFilterFocused) {
			if (event.key === 'Escape') {
				filterInput?.blur();
			} else if (event.key === 'Enter') {
				event.preventDefault();
				runSelected();
			} else if (event.key === 'ArrowDown') {
				event.preventDefault();
				moveSelection(1);
			} else if (event.key === 'ArrowUp') {
				event.preventDefault();
				moveSelection(-1);
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
				runSelected();
				break;
			case '/':
				event.preventDefault();
				filterInput?.focus();
				break;
		}
	}

	async function createNote() {
		const promptLabel = currentFolder
			? `Title for the new note in ${currentFolder}:`
			: 'Title for the new note:';
		const title = window.prompt(promptLabel);
		if (!title || !title.trim()) return;
		creating = true;
		createError = null;
		try {
			const idOrTitle = currentFolder ? `${currentFolder}/${title.trim()}` : title.trim();
			const meta = await apiPost<NoteMeta>('/api/notes', { id_or_title: idOrTitle });
			await goto(resolve(`/notes/${encodeNotePath(meta.id)}`));
		} catch (err) {
			if (err instanceof ApiError && err.status === 401) {
				flagLoginRequired();
				await goto(resolve('/login'));
				return;
			}
			if (err instanceof ApiError && err.status === 0) {
				createError = "You're offline — creating notes requires a connection.";
				return;
			}
			const message = err instanceof Error ? err.message : 'Failed to create note';
			window.alert(message);
		} finally {
			creating = false;
		}
	}

	function createFolder() {
		const promptLabel = currentFolder
			? `Name for the new folder inside ${currentFolder}:`
			: 'Name for the new folder:';
		const name = window.prompt(promptLabel);
		if (!name || !name.trim()) return;
		const slug = slugifySegment(name.trim());
		if (!slug) {
			window.alert('Invalid folder name — use letters, numbers, or hyphens.');
			return;
		}
		const newPath = currentFolder ? `${currentFolder}/${slug}` : slug;
		setFolder(newPath);
	}

	// Chip colour for each item group.
	function chipColor(item: PaletteItem): 'accent' | 'orange' | 'danger' | 'dim' {
		if (item.destructive) return 'danger';
		if (item.group === 'action') return 'accent';
		if (item.group === 'folder') return 'orange';
		return 'dim';
	}
</script>

<svelte:window onkeydown={onKeydown} />

{#snippet slashHint()}/{/snippet}

<div class="notes-page">
	<Card class="notes-card">
		<div class="notes-header">
			<SectionTitle>
				{currentFolder ? (currentFolder.split('/').pop() ?? currentFolder) : 'Vault'}
			</SectionTitle>
			<div class="header-actions">
				<Button variant="outline" size="sm" onclick={createFolder}>+ New folder</Button>
				<Button variant="primary" size="sm" onclick={createNote} disabled={creating}>
					{creating ? 'Creating…' : '+ New note'}
				</Button>
			</div>
		</div>

		{#if createError}
			<p class="status-line error">{createError}</p>
		{/if}

		{#if offlineCachedAt !== null}
			<p class="offline-banner">
				offline — showing cached list ({relativeAge(offlineCachedAt)})
			</p>
		{/if}

		{#if currentFolder}
			<Breadcrumb crumbs={breadcrumbs} onnavigate={setFolder} />
		{/if}

		<div class="filter-row">
			<Input
				type="text"
				placeholder="Search commands and notes…"
				bind:value={filter}
				bind:el={filterInput}
				suffix={IS_APP ? undefined : slashHint}
			>
				{#snippet prefix()}&gt;{/snippet}
			</Input>
		</div>

		{#if filter && currentFolder}
			<div class="scope-row">
				<button
					type="button"
					class="scope-btn"
					onclick={() => {
						searchScope = searchScope === 'folder' ? 'all' : 'folder';
					}}
				>
					<Chip color={searchScope === 'folder' ? 'accent' : 'dim'} variant="outline">
						{searchScope === 'folder'
							? `in ${currentFolder.split('/').pop()}`
							: 'all notes'} ×
					</Chip>
				</button>
			</div>
		{/if}

		{#if loading}
			<p class="status-line">Loading notes…</p>
		{:else if loadError}
			<p class="status-line error">Error loading notes: {loadError}</p>
		{:else if currentFolder && !filter && folderFolders.length === 0 && folderNotes.length === 0}
			<div class="empty-folder">
				<p class="status-line">This folder is empty.</p>
				<Button variant="outline" size="sm" onclick={leaveFolder}>← Leave folder</Button>
			</div>
		{:else if items.length === 0}
			<p class="status-line">Nothing matches "{filter}".</p>
		{:else}
			<ul class="notes-list" bind:this={listEl}>
				{#each items as item, index (item.id)}
					<li>
						<button
							type="button"
							class="row"
							data-selected={index === selectedIndex}
							class:selected={index === selectedIndex}
							class:destructive={item.destructive === true}
							class:folder-row={item.group === 'folder'}
							onmouseenter={() => (selectedIndex = index)}
							onclick={() => void item.run()}
						>
							<span class="row-label" title={item.label}
								>{item.label.trim() ? item.label : '(untitled)'}</span
							>
							{#if item.hint}
								<span class="row-hint" title={item.hint}>{item.hint}</span>
							{/if}
							<span class="row-chip">
								<Chip color={chipColor(item)} variant="outline">
									{item.group}
								</Chip>
							</span>
						</button>
					</li>
				{/each}
			</ul>

			<p class="notes-count">— {noteCount} note{noteCount === 1 ? '' : 's'} —</p>
		{/if}

		<p class="hint">
			Keyboard: <kbd>j</kbd>/<kbd>k</kbd> or arrows to move, <kbd>Enter</kbd> to run, <kbd>/</kbd> to
			search.
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

	.header-actions {
		display: flex;
		align-items: center;
		gap: var(--space-3);
		flex-shrink: 0;
	}

	.filter-row {
		margin-bottom: var(--space-4);
	}

	.scope-row {
		margin-bottom: var(--space-4);
	}

	.scope-btn {
		background: none;
		border: none;
		padding: 0;
		cursor: pointer;
	}

	.scope-btn:hover {
		opacity: 0.75;
	}

	.notes-list {
		list-style: none;
		margin: 0;
		padding: 0;
		border-top: 1px solid var(--border-default);
	}

	.row {
		display: flex;
		justify-content: flex-start;
		align-items: baseline;
		gap: var(--space-6);
		width: 100%;
		padding: var(--space-4) var(--space-3);
		text-align: left;
		background: transparent;
		color: inherit;
		min-width: 0;
		border: none;
		border-left: 2px solid transparent;
		border-bottom: 1px solid var(--border-default);
		font-family: var(--font-term);
		font-size: var(--type-data);
		cursor: pointer;
		transition:
			background 120ms linear,
			border-color 120ms linear;
	}

	.row:hover,
	.row.selected {
		background: rgba(121, 242, 121, 0.07);
		border-left: 2px solid var(--kv-accent);
	}

	.row.folder-row:hover,
	.row.folder-row.selected {
		background: rgba(255, 165, 0, 0.06);
		border-left-color: var(--kv-orange);
	}

	.row.destructive .row-label {
		color: var(--kv-danger);
	}

	.row.destructive:hover,
	.row.destructive.selected {
		background: color-mix(in srgb, var(--kv-danger) 8%, transparent);
		border-left-color: var(--kv-danger);
	}

	.row-label {
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		min-width: 0;
		flex: 1 1 auto;
		color: var(--kv-ink);
	}

	.row-hint {
		color: var(--kv-dim);
		font-size: var(--type-meta);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		flex: 0 1 auto;
		min-width: 0;
	}

	.row-chip {
		flex: 0 0 auto;
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

	.empty-folder {
		display: flex;
		align-items: center;
		gap: var(--space-5);
		flex-wrap: wrap;
	}

	.empty-folder .status-line {
		margin: 0;
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
