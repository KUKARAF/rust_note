<script lang="ts">
	// Command palette: a global search bar over actions (log in/out, settings,
	// app-only folder actions) and every note. Opened from the root layout via
	// Ctrl/Cmd+K or a swipe-down from the top of the screen; context-aware —
	// actions that make no sense in the current auth state simply don't exist
	// (e.g. "Log in" disappears once logged in).
	import { tick } from 'svelte';
	import { apiGet } from '$lib/api/client';
	import { IS_APP } from '$lib/api/deviceToken';
	import { auth } from '$lib/stores/auth';
	import { readNotesListCache } from '$lib/stores/offline';
	import { buildActions, filterItems, type NoteMeta, type PaletteItem } from './items';
	import Card from '$lib/design/Card.svelte';
	import Chip from '$lib/design/Chip.svelte';
	import Input from '$lib/design/Input.svelte';

	let {
		open = false,
		onclose
	}: {
		open?: boolean;
		onclose: () => void;
	} = $props();

	let query = $state('');
	let selectedIndex = $state(0);
	let notes = $state<NoteMeta[]>([]);
	let inputEl: HTMLInputElement | undefined = $state();
	let listEl: HTMLUListElement | undefined = $state();

	// Refresh the note pool and reset the palette every time it opens. The
	// fetch is fire-and-forget: the actions render immediately and notes pop
	// in when the request lands (or from the offline cache when it doesn't).
	$effect(() => {
		if (!open) return;
		query = '';
		selectedIndex = 0;
		void tick().then(() => inputEl?.focus());
		if ($auth.user === null) {
			notes = [];
			return;
		}
		void apiGet<NoteMeta[]>('/api/notes')
			.then((fetched) => {
				notes = fetched;
			})
			.catch(() => {
				// Any failure (offline, transient server error) falls back to the
				// last cached list — same source the notes page uses offline.
				notes = readNotesListCache()?.notes ?? [];
			});
	});

	// Actions + matching notes come from the shared palette module, so this
	// modal and the /notes inline search stay one implementation.
	const items = $derived<PaletteItem[]>(
		filterItems(query, buildActions({ user: $auth.user }), notes)
	);

	// Keep the selection in range whenever the item list changes.
	$effect(() => {
		if (selectedIndex >= items.length) {
			selectedIndex = Math.max(0, items.length - 1);
		}
	});

	async function runItem(item: PaletteItem) {
		onclose();
		await item.run();
	}

	function moveSelection(delta: number) {
		if (items.length === 0) return;
		selectedIndex = Math.min(Math.max(selectedIndex + delta, 0), items.length - 1);
		void tick().then(() => {
			listEl?.querySelector<HTMLElement>('[data-selected="true"]')?.scrollIntoView({
				block: 'nearest'
			});
		});
	}

	// Handled at the palette root (not on window) and always stopped from
	// propagating: page-level <svelte:window> handlers (the notes list's j/k
	// navigation, the editor's Ctrl+S hint) must never react to keystrokes
	// meant for the palette.
	function onRootKeydown(event: KeyboardEvent) {
		event.stopPropagation();
		switch (event.key) {
			case 'Escape':
				event.preventDefault();
				onclose();
				break;
			case 'ArrowDown':
				event.preventDefault();
				moveSelection(1);
				break;
			case 'ArrowUp':
				event.preventDefault();
				moveSelection(-1);
				break;
			case 'Enter': {
				event.preventDefault();
				const item = items[selectedIndex];
				if (item) void runItem(item);
				break;
			}
		}
	}

	function onBackdropClick(event: MouseEvent) {
		if (event.target === event.currentTarget) onclose();
	}
</script>

{#if open}
	<div
		class="palette-backdrop"
		onclick={onBackdropClick}
		onkeydown={onRootKeydown}
		role="presentation"
	>
		<div class="palette-wrap" role="dialog" aria-modal="true" aria-label="Command palette">
			<Card>
				<div class="palette">
					{#if IS_APP}
						<!-- No keyboard-shortcut tooltips on touch: no esc suffix. -->
						<Input
							type="text"
							placeholder="Search commands and notes…"
							bind:value={query}
							bind:el={inputEl}
						>
							{#snippet prefix()}&gt;{/snippet}
						</Input>
					{:else}
						<Input
							type="text"
							placeholder="Search commands and notes…"
							bind:value={query}
							bind:el={inputEl}
						>
							{#snippet prefix()}&gt;{/snippet}
							{#snippet suffix()}esc{/snippet}
						</Input>
					{/if}

					{#if items.length === 0}
						<p class="palette-empty">Nothing matches "{query}".</p>
					{:else}
						<ul class="palette-list" bind:this={listEl}>
							{#each items as item, index (item.id)}
								<li>
									<button
										type="button"
										class="palette-item"
										data-selected={index === selectedIndex}
										onclick={() => void runItem(item)}
										onmousemove={() => (selectedIndex = index)}
									>
										<span class="palette-item-label">{item.label}</span>
										{#if item.hint}
											<span class="palette-item-hint">{item.hint}</span>
										{/if}
										<span class="palette-item-chip">
											<Chip color={item.group === 'action' ? 'accent' : 'dim'} variant="outline">
												{item.group}
											</Chip>
										</span>
									</button>
								</li>
							{/each}
						</ul>
					{/if}

					{#if !IS_APP}
						<p class="palette-footer">↑↓ navigate · enter run · esc close</p>
					{/if}
				</div>
			</Card>
		</div>
	</div>
{/if}

<style>
	.palette-backdrop {
		position: fixed;
		inset: 0;
		background: rgba(3, 6, 3, 0.72);
		display: flex;
		align-items: flex-start;
		justify-content: center;
		/* Above the z-100 dialogs (Share, MirrorFolder) — the palette is the
		   topmost global surface. Safe-area padding keeps it clear of the
		   Android status/navigation bars in the edge-to-edge app. */
		z-index: 200;
		padding: calc(var(--safe-top) + 12vh) calc(var(--space-6) + var(--safe-right))
			calc(var(--space-9) + var(--safe-bottom)) calc(var(--space-6) + var(--safe-left));
		overflow-y: auto;
	}

	.palette-wrap {
		width: 100%;
		max-width: 34rem;
	}

	.palette {
		display: flex;
		flex-direction: column;
		gap: var(--space-4);
		padding: var(--space-5);
	}

	.palette-list {
		list-style: none;
		margin: 0;
		padding: 0;
		display: flex;
		flex-direction: column;
		max-height: 50vh;
		overflow-y: auto;
	}

	.palette-item {
		display: flex;
		align-items: baseline;
		gap: var(--space-4);
		width: 100%;
		text-align: left;
		background: transparent;
		border: none;
		border-left: 2px solid transparent;
		padding: var(--space-3) var(--space-4);
		cursor: pointer;
		font-family: var(--font-term);
		font-size: var(--type-body);
		color: var(--kv-ink);
	}

	.palette-item[data-selected='true'] {
		background: color-mix(in srgb, var(--kv-accent) 10%, transparent);
		border-left-color: var(--kv-accent);
	}

	.palette-item-label {
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}

	.palette-item-hint {
		flex: 1;
		min-width: 0;
		color: var(--kv-dim);
		font-size: var(--type-meta);
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}

	.palette-item-chip {
		margin-left: auto;
		flex-shrink: 0;
	}

	.palette-empty {
		margin: 0;
		padding: var(--space-4);
		font-family: var(--font-term);
		color: var(--kv-dim);
	}

	.palette-footer {
		margin: 0;
		font-family: var(--font-pixel);
		font-size: var(--type-chip);
		text-transform: uppercase;
		letter-spacing: var(--tracking-pixel);
		color: var(--kv-dim);
	}
</style>
