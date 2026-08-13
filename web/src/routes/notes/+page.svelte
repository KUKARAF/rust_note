<script lang="ts">
	// Note list view — doubling as the inline command search: the search box
	// filters commands AND notes in one unified list (same model as the modal
	// command palette, via $lib/commandPalette/items). Static-shaped route, so —
	// consistent with adapter-static/ssr=false — data is fetched client-side on
	// mount rather than via a `+page.ts` load function.
	import { onMount, tick } from 'svelte';
	import { goto } from '$app/navigation';
	import { resolve } from '$app/paths';
	import { apiGet, apiPost, ApiError } from '$lib/api/client';
	import { cacheNotesList, readNotesListCache } from '$lib/stores/offline';
	import { auth, flagLoginRequired } from '$lib/stores/auth';
	import { IS_APP } from '$lib/api/deviceToken';
	import { encodeNotePath } from '$lib/notes/path';
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

	// Unified command + note list, from the same builder the modal palette uses.
	// No note cap here (Infinity) — this is the full vault, not a launcher.
	const items = $derived<PaletteItem[]>(
		filterItems(filter, buildActions({ user: $auth.user }), notes, Infinity)
	);
	const noteCount = $derived(items.filter((i) => i.group === 'note').length);

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
				flagLoginRequired();
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

		// Let the layout's swipe-down gesture focus THIS search (there's no
		// modal palette needed while the inline search is on screen).
		notesSearchFocuser.set(() => filterInput?.focus());

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
			notesSearchFocuser.set(null);
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

	// Keep the selection in range whenever the item list changes.
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
		const title = window.prompt('Title for the new note:');
		if (!title || !title.trim()) return;
		creating = true;
		createError = null;
		try {
			const meta = await apiPost<NoteMeta>('/api/notes', { id_or_title: title.trim() });
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
</script>

<svelte:window onkeydown={onKeydown} />

<!-- The '/' suffix is a keyboard-shortcut hint — meaningless on touch, so
     the app build omits it (passed conditionally to the filter Input). -->
{#snippet slashHint()}/{/snippet}

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

		{#if loading}
			<p class="status-line">Loading notes…</p>
		{:else if loadError}
			<p class="status-line error">Error loading notes: {loadError}</p>
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
								<Chip color={item.group === 'action' ? 'accent' : 'dim'} variant="outline">
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

	.filter-row {
		margin-bottom: var(--space-6);
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
