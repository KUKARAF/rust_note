<script lang="ts">
	// Aggregated todo board. Static-shaped route → client-side fetch on mount
	// (same pattern as /notes). Reads come from GET /api/todos; toggling a
	// checkbox writes back through the collab CRDT (see $lib/notes/todoToggle).
	import { onMount } from 'svelte';
	import { goto } from '$app/navigation';
	import { resolve } from '$app/paths';
	import { apiGet, apiPost, ApiError } from '$lib/api/client';
	import { auth, flagLoginRequired } from '$lib/stores/auth';
	import { encodeNotePath } from '$lib/notes/path';
	import Card from '$lib/design/Card.svelte';
	import Button from '$lib/design/Button.svelte';
	import Input from '$lib/design/Input.svelte';
	import SectionTitle from '$lib/design/SectionTitle.svelte';
	import Chip from '$lib/design/Chip.svelte';
	import {
		applyQuery,
		ageInDays,
		isSpoiling,
		BURNER_META,
		type Todo,
		type QuerySpec,
		type SortField,
		type Burner
	} from '$lib/notes/todos';
	import { setTodoDone, collabUserFrom } from '$lib/notes/todoToggle';

	const now = new Date();

	let todos = $state<Todo[]>([]);
	let loading = $state(true);
	let loadError = $state<string | null>(null);
	let spec = $state<QuerySpec>({ status: 'all' });
	// Free-text filter, kept separate so it can bind to the Input (which needs a
	// plain string). Folded into the spec via `effectiveSpec`.
	let textFilter = $state('');

	// AI natural-language query box.
	let nl = $state('');
	let asking = $state(false);
	let aiError = $state<string | null>(null);
	let toggleError = $state<string | null>(null);

	const effectiveSpec = $derived<QuerySpec>({
		...spec,
		text: textFilter.trim() === '' ? undefined : textFilter.trim()
	});
	const result = $derived(applyQuery(todos, effectiveSpec, now));

	const SORTS: { field: SortField; label: string; defaultDir: 'asc' | 'desc' }[] = [
		{ field: 'date', label: 'Date', defaultDir: 'desc' },
		{ field: 'pomodoros', label: 'Pomodoros', defaultDir: 'desc' },
		{ field: 'start', label: 'Start', defaultDir: 'asc' },
		{ field: 'due', label: 'Due', defaultDir: 'asc' }
	];

	const activeSort = $derived(spec.sort?.[0]);

	function setSort(field: SortField, defaultDir: 'asc' | 'desc') {
		const cur = spec.sort?.[0];
		if (cur?.field === field) {
			spec.sort = [{ field, dir: cur.dir === 'asc' ? 'desc' : 'asc' }];
		} else {
			spec.sort = [{ field, dir: defaultDir }];
		}
	}

	function toggleBurner(b: Burner) {
		const set = new Set(spec.burners ?? []);
		if (set.has(b)) set.delete(b);
		else set.add(b);
		spec.burners = set.size ? [...set] : undefined;
	}

	// Locations are an open vocabulary discovered from the loaded todos, so the
	// filter offers exactly the contexts in use (sorted), not a fixed list.
	const availableLocations = $derived(
		[...new Set(todos.flatMap((t) => t.locations))].sort((a, b) => a.localeCompare(b))
	);

	function toggleLocation(l: string) {
		const set = new Set(spec.locations ?? []);
		if (set.has(l)) set.delete(l);
		else set.add(l);
		spec.locations = set.size ? [...set] : undefined;
	}

	function setStatus(status: 'open' | 'all' | 'done') {
		spec.status = status;
	}

	function resetSpec() {
		spec = { status: 'all' };
		textFilter = '';
		nl = '';
		aiError = null;
	}

	async function loadTodos() {
		loading = true;
		loadError = null;
		try {
			todos = await apiGet<Todo[]>('/api/todos');
		} catch (err) {
			if (err instanceof ApiError && err.status === 401) {
				flagLoginRequired();
				await goto(resolve('/login'));
				return;
			}
			if (err instanceof ApiError && err.status === 0) {
				loadError = "You're offline — the todo board needs a connection to aggregate notes.";
				return;
			}
			loadError = err instanceof Error ? err.message : 'Failed to load todos';
		} finally {
			loading = false;
		}
	}

	onMount(() => {
		void loadTodos();
	});

	async function runQuery() {
		if (nl.trim() === '') return;
		asking = true;
		aiError = null;
		try {
			const produced = await apiPost<QuerySpec>('/api/todos/query', { nl: nl.trim() });
			// Preserve nothing from the old spec — the LLM produces a full one.
			spec = produced ?? { status: 'all' };
			textFilter = produced?.text ?? '';
		} catch (err) {
			if (err instanceof ApiError && err.status === 400) {
				aiError = 'Add an OpenRouter API key in Settings to use natural-language queries.';
			} else if (err instanceof ApiError && err.status === 0) {
				aiError = "You're offline — natural-language queries need a connection.";
			} else {
				aiError = err instanceof Error ? err.message : 'Query failed';
			}
		} finally {
			asking = false;
		}
	}

	async function toggle(todo: Todo) {
		const user = $auth.user;
		if (!user) {
			flagLoginRequired();
			await goto(resolve('/login'));
			return;
		}
		toggleError = null;
		const want = !todo.done;
		todo.done = want; // optimistic
		try {
			await setTodoDone(todo, collabUserFrom(user), want);
		} catch (err) {
			todo.done = !want; // revert
			toggleError = err instanceof Error ? err.message : 'Failed to update the task.';
		}
	}

	function openNote(todo: Todo) {
		void goto(resolve(`/notes/${encodeNotePath(todo.note_id)}`));
	}

	// Editable summary of the active query, so the AI/button state is visible
	// and each facet can be cleared individually.
	interface Facet {
		label: string;
		clear: () => void;
	}
	const facets = $derived.by<Facet[]>(() => {
		const out: Facet[] = [];
		if (textFilter.trim())
			out.push({ label: `text: "${textFilter.trim()}"`, clear: () => (textFilter = '') });
		for (const b of spec.burners ?? [])
			out.push({ label: BURNER_META[b].label, clear: () => toggleBurner(b) });
		for (const t of spec.tags ?? [])
			out.push({
				label: `#${t}`,
				clear: () => (spec.tags = (spec.tags ?? []).filter((x) => x !== t))
			});
		for (const l of spec.locations ?? [])
			out.push({ label: `@${l}`, clear: () => toggleLocation(l) });
		if (spec.status && spec.status !== 'all')
			out.push({ label: spec.status, clear: () => (spec.status = 'all') });
		if (spec.pomodorosMin != null)
			out.push({ label: `≥${spec.pomodorosMin}p`, clear: () => (spec.pomodorosMin = undefined) });
		if (spec.pomodorosMax != null)
			out.push({ label: `≤${spec.pomodorosMax}p`, clear: () => (spec.pomodorosMax = undefined) });
		if (spec.sort?.[0])
			out.push({
				label: `sort ${spec.sort[0].field} ${spec.sort[0].dir === 'asc' ? '↑' : '↓'}`,
				clear: () => (spec.sort = undefined)
			});
		return out;
	});
</script>

<div class="todo-page">
	<Card class="todo-card">
		<div class="todo-header">
			<SectionTitle>Todos</SectionTitle>
			<div class="header-actions">
				<span class="count">{result.openTotal} open</span>
				<Button variant="outline" size="sm" onclick={loadTodos}>Refresh</Button>
			</div>
		</div>

		<!-- Natural-language query -->
		<div class="ai-row">
			<Input
				type="text"
				placeholder="Ask: “fridge stuff, most pomodoros first”…"
				bind:value={nl}
			>
				{#snippet prefix()}✦{/snippet}
			</Input>
			<Button variant="primary" size="sm" onclick={runQuery} disabled={asking || nl.trim() === ''}>
				{asking ? 'Asking…' : 'Ask'}
			</Button>
		</div>
		{#if aiError}
			<p class="status-line error">{aiError}</p>
		{/if}

		<!-- Manual controls -->
		<div class="controls">
			<div class="control-group">
				<span class="control-label">sort</span>
				{#each SORTS as s (s.field)}
					<button
						type="button"
						class="pill"
						class:active={activeSort?.field === s.field}
						onclick={() => setSort(s.field, s.defaultDir)}
					>
						{s.label}{activeSort?.field === s.field
							? activeSort.dir === 'asc'
								? ' ↑'
								: ' ↓'
							: ''}
					</button>
				{/each}
			</div>

			<div class="control-group">
				<span class="control-label">show</span>
				{#each ['open', 'all', 'done'] as const as st (st)}
					<button
						type="button"
						class="pill"
						class:active={(spec.status ?? 'all') === st}
						onclick={() => setStatus(st)}>{st}</button
					>
				{/each}
			</div>

			<div class="control-group">
				<span class="control-label">burner</span>
				{#each ['frontburner', 'backburner', 'fridge', 'oven'] as const as b (b)}
					<button
						type="button"
						class="pill"
						class:active={spec.burners?.includes(b)}
						onclick={() => toggleBurner(b)}>{BURNER_META[b].glyph} {BURNER_META[b].label}</button
					>
				{/each}
			</div>

			{#if availableLocations.length > 0}
				<div class="control-group">
					<span class="control-label">location</span>
					{#each availableLocations as loc (loc)}
						<button
							type="button"
							class="pill"
							class:active={spec.locations?.includes(loc)}
							onclick={() => toggleLocation(loc)}>@{loc}</button
						>
					{/each}
				</div>
			{/if}

			<div class="control-group filter-input">
				<Input type="text" placeholder="filter text…" bind:value={textFilter}>
					{#snippet prefix()}/{/snippet}
				</Input>
			</div>
		</div>

		{#if facets.length > 0}
			<div class="facets">
				{#each facets as f (f.label)}
					<button type="button" class="facet" onclick={f.clear} title="remove">
						{f.label} ✕
					</button>
				{/each}
				<button type="button" class="facet reset" onclick={resetSpec}>reset</button>
			</div>
		{/if}

		{#if toggleError}
			<p class="status-line error">{toggleError}</p>
		{/if}

		<!-- Board -->
		{#if loading}
			<p class="status-line">Loading todos…</p>
		{:else if loadError}
			<p class="status-line error">{loadError}</p>
		{:else if todos.length === 0}
			<p class="status-line">No tasks found in your daily notes yet.</p>
		{:else if result.groups.length === 0}
			<p class="status-line">No tasks match the current query.</p>
		{:else}
			{#each result.groups as group (group.burner)}
				<section class="burner">
					<header class="burner-head burner-{group.meta.color}">
						<span class="burner-glyph">{group.meta.glyph}</span>
						<span class="burner-name">{group.meta.label}</span>
						<span class="burner-hint">{group.meta.hint}</span>
						<span class="burner-count">{group.openCount}</span>
					</header>
					<ul class="todo-list">
						{#each group.todos as todo (todo.note_id + ':' + todo.line)}
							<li
								class="todo-row"
								class:done={todo.done}
								style="--depth: {todo.depth};"
							>
								<button
									type="button"
									class="check"
									class:checked={todo.done}
									aria-label={todo.done ? 'Mark not done' : 'Mark done'}
									onclick={() => toggle(todo)}
								>
									{todo.done ? '✓' : ''}
								</button>
								<button type="button" class="todo-text" onclick={() => openNote(todo)}>
									<span class="text">{todo.text_clean || '(empty task)'}</span>
									<span class="meta">
										{#if todo.pomodoros != null}<span class="tag pom">{todo.pomodoros}p</span>{/if}
										{#if todo.start}<span class="tag">@{todo.start}</span>{/if}
										{#if todo.due}<span class="tag due">due:{todo.due}</span>{/if}
										{#if todo.tags.length}<span class="tag hash">#{todo.tags.join(' #')}</span>{/if}
										{#if todo.locations.length}<span class="tag loc"
												>@{todo.locations.join(' @')}</span
											>{/if}
										{#if isSpoiling(todo, now)}<span class="tag spoiling"
												>spoiling · {ageInDays(todo.date, now)}d</span
											>{/if}
										<span class="tag src">{todo.date ?? todo.note_id}</span>
									</span>
								</button>
							</li>
						{/each}
					</ul>
				</section>
			{/each}
		{/if}
	</Card>
</div>

<style>
	.todo-page {
		max-width: 46rem;
		margin: 0 auto;
	}

	:global(.todo-card) {
		display: flex;
		flex-direction: column;
	}

	.todo-header {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: var(--space-6);
		margin-bottom: var(--space-5);
	}

	.header-actions {
		display: flex;
		align-items: center;
		gap: var(--space-4);
	}

	.count {
		font-family: var(--font-term);
		font-size: var(--type-meta);
		color: var(--kv-dim);
	}

	.ai-row {
		display: flex;
		gap: var(--space-4);
		align-items: stretch;
		margin-bottom: var(--space-4);
	}

	.ai-row :global(.kv-input-wrap),
	.filter-input {
		flex: 1 1 auto;
	}

	.controls {
		display: flex;
		flex-wrap: wrap;
		gap: var(--space-5);
		align-items: center;
		margin-bottom: var(--space-4);
	}

	.control-group {
		display: flex;
		align-items: center;
		gap: var(--space-2);
		flex-wrap: wrap;
	}

	.control-label {
		font-family: var(--font-pixel);
		font-size: var(--type-chip);
		text-transform: uppercase;
		letter-spacing: var(--tracking-pixel);
		color: var(--kv-dim);
		margin-right: var(--space-2);
	}

	.pill {
		background: transparent;
		border: 1px solid var(--border-default);
		border-radius: var(--radius-control);
		color: var(--kv-dim);
		font-family: var(--font-term);
		font-size: var(--type-meta);
		padding: 3px 8px;
		cursor: pointer;
		transition:
			border-color 120ms linear,
			color 120ms linear;
	}

	.pill:hover {
		color: var(--kv-ink);
		border-color: var(--kv-accent);
	}

	.pill.active {
		color: var(--kv-accent);
		border-color: var(--kv-accent);
		background: rgba(121, 242, 121, 0.08);
	}

	.facets {
		display: flex;
		flex-wrap: wrap;
		gap: var(--space-2);
		margin-bottom: var(--space-5);
	}

	.facet {
		background: rgba(121, 242, 121, 0.08);
		border: 1px solid var(--border-accent);
		border-radius: var(--radius-control);
		color: var(--kv-ink);
		font-family: var(--font-term);
		font-size: var(--type-meta);
		padding: 2px 7px;
		cursor: pointer;
	}

	.facet.reset {
		background: transparent;
		border-color: var(--border-default);
		color: var(--kv-dim);
	}

	.burner {
		margin-top: var(--space-5);
	}

	.burner-head {
		display: flex;
		align-items: baseline;
		gap: var(--space-3);
		padding: var(--space-3) 0;
		border-bottom: 1px solid var(--border-default);
	}

	.burner-glyph {
		font-size: var(--type-body);
	}

	.burner-name {
		font-family: var(--font-pixel);
		font-size: var(--type-label);
		text-transform: uppercase;
		letter-spacing: var(--tracking-pixel);
	}

	.burner-hint {
		font-family: var(--font-term);
		font-size: var(--type-meta);
		color: var(--kv-dim);
		flex: 1 1 auto;
	}

	.burner-count {
		font-family: var(--font-term);
		font-size: var(--type-meta);
		color: var(--kv-dim);
	}

	.burner-danger .burner-glyph,
	.burner-danger .burner-name {
		color: var(--kv-danger);
	}
	.burner-orange .burner-glyph,
	.burner-orange .burner-name {
		color: var(--kv-orange);
	}
	.burner-accent .burner-glyph,
	.burner-accent .burner-name {
		color: var(--kv-accent);
	}
	.burner-dim .burner-glyph,
	.burner-dim .burner-name {
		color: var(--kv-dim);
	}

	.todo-list {
		list-style: none;
		margin: 0;
		padding: 0;
	}

	.todo-row {
		display: flex;
		align-items: flex-start;
		gap: var(--space-3);
		padding: var(--space-3) 0 var(--space-3) calc(var(--depth) * var(--space-6));
		border-bottom: 1px solid var(--border-default);
	}

	.todo-row.done {
		opacity: 0.5;
	}

	.check {
		flex: 0 0 auto;
		width: 18px;
		height: 18px;
		margin-top: 2px;
		background: var(--surface-input);
		border: 1px solid var(--border-default);
		border-radius: var(--radius-control);
		color: var(--kv-accent-ink);
		font-size: 12px;
		line-height: 1;
		display: flex;
		align-items: center;
		justify-content: center;
		cursor: pointer;
	}

	.check.checked {
		background: var(--kv-accent);
		border-color: var(--kv-accent);
	}

	.todo-text {
		flex: 1 1 auto;
		min-width: 0;
		display: flex;
		flex-direction: column;
		gap: 3px;
		background: transparent;
		border: none;
		text-align: left;
		cursor: pointer;
		padding: 0;
		font: inherit;
	}

	.todo-row.done .text {
		text-decoration: line-through;
	}

	.text {
		font-family: var(--font-term);
		font-size: var(--type-data);
		color: var(--kv-ink);
	}

	.meta {
		display: flex;
		flex-wrap: wrap;
		gap: var(--space-2);
	}

	.tag {
		font-family: var(--font-term);
		font-size: var(--type-meta);
		color: var(--kv-dim);
	}

	.tag.pom {
		color: var(--kv-orange);
	}
	.tag.due {
		color: var(--kv-danger);
	}
	.tag.hash {
		color: var(--kv-accent);
	}
	.tag.loc {
		color: var(--kv-blue, var(--kv-accent));
	}
	.tag.spoiling {
		color: var(--kv-danger);
	}
	.tag.src {
		color: var(--kv-faint);
	}

	.status-line {
		font-family: var(--font-term);
		font-size: var(--type-body);
		color: var(--kv-dim);
	}

	.status-line.error {
		color: var(--kv-danger);
	}
</style>
