<script lang="ts">
	// Aggregated todo board — Todoist-calm redesign. One search/command bar plus a
	// Display popover (grouping/sort/show); no persistent filter pills. Color is
	// rationed to priority (checkbox ring / group dot) and the spoiling cue.
	// Reads come from GET /api/todos; toggling writes back through the collab CRDT.
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
	import {
		applyQuery,
		parseSearch,
		ageInDays,
		isSpoiling,
		relativeDate,
		BURNER_COLOR,
		type Todo,
		type QuerySpec,
		type SortField,
		type SortKey,
		type StatusFilter,
		type GroupMode
	} from '$lib/notes/todos';
	import { setTodoDone, collabUserFrom } from '$lib/notes/todoToggle';

	const now = new Date();

	let todos = $state<Todo[]>([]);
	let loading = $state(true);
	let loadError = $state<string | null>(null);
	let toggleError = $state<string | null>(null);

	// The single search/command bar: free text plus literal tokens (@loc, #tag,
	// burner names, p>=N / p<=N), parsed by `parseSearch`. The AI query writes
	// its result back into this same bar as tokens, so there is one source of
	// truth and the two paths can't fight.
	let search = $state('');
	let asking = $state(false);
	let aiError = $state<string | null>(null);

	// Display popover — grouping, sort and completed-visibility live here, off the
	// resting canvas. Default: group by burner, show open only (done hidden).
	let displayOpen = $state(false);
	let groupMode = $state<GroupMode>('burner');
	let status = $state<StatusFilter>('open');
	let sort = $state<SortKey[] | undefined>(undefined);

	const effectiveSpec = $derived<QuerySpec>({ ...parseSearch(search), status, sort });
	const result = $derived(applyQuery(todos, effectiveSpec, now, groupMode));

	/** True when anything is narrowing the view (so we offer a clear affordance). */
	const filtered = $derived(search.trim() !== '' || status !== 'open' || sort != null);

	const SORTS: { field: SortField; label: string; defaultDir: 'asc' | 'desc' }[] = [
		{ field: 'date', label: 'Date', defaultDir: 'desc' },
		{ field: 'pomodoros', label: 'Pomodoros', defaultDir: 'desc' },
		{ field: 'start', label: 'Start', defaultDir: 'asc' },
		{ field: 'due', label: 'Due', defaultDir: 'asc' }
	];
	const activeSort = $derived(sort?.[0]);

	function setSort(field: SortField, defaultDir: 'asc' | 'desc') {
		const cur = sort?.[0];
		sort =
			cur?.field === field
				? [{ field, dir: cur.dir === 'asc' ? 'desc' : 'asc' }]
				: [{ field, dir: defaultDir }];
	}

	function ringColor(t: Todo): string {
		return t.burner ? BURNER_COLOR[t.burner] : 'none';
	}

	/** Render a spec back into search-bar tokens (the AI's answer becomes editable text). */
	function specToSearch(s: QuerySpec): string {
		const parts: string[] = [];
		for (const b of s.burners ?? []) parts.push(b);
		for (const t of s.tags ?? []) parts.push(`#${t}`);
		for (const l of s.locations ?? []) parts.push(`@${l}`);
		if (s.pomodorosMin != null) parts.push(`p>=${s.pomodorosMin}`);
		if (s.pomodorosMax != null) parts.push(`p<=${s.pomodorosMax}`);
		if (s.text) parts.push(s.text);
		return parts.join(' ');
	}

	function clearAll() {
		search = '';
		status = 'open';
		sort = undefined;
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
		const q = search.trim();
		if (q === '') return;
		asking = true;
		aiError = null;
		try {
			const produced = await apiPost<QuerySpec>('/api/todos/query', { nl: q });
			// Fold the AI's answer back into the one search bar (editable tokens).
			search = specToSearch(produced ?? {});
			if (produced?.status) status = produced.status;
			if (produced?.sort) sort = produced.sort;
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

		<!-- One search/command bar + a Display button. No persistent filter pills. -->
		<div class="toolbar">
			<div class="search">
				<Input type="text" placeholder="Search or filter — try “fridge @home p>=2”…" bind:value={search}>
					{#snippet prefix()}/{/snippet}
				</Input>
			</div>
			{#if filtered}
				<button type="button" class="display-btn" aria-label="Clear filters" onclick={clearAll}>✕</button>
			{/if}
			<Button
				variant="outline"
				size="sm"
				onclick={runQuery}
				disabled={asking || search.trim() === ''}
			>
				{asking ? '…' : '✦ Ask'}
			</Button>
			<button
				type="button"
				class="display-btn"
				class:active={displayOpen}
				aria-label="Display options"
				onclick={() => (displayOpen = !displayOpen)}>⚙</button
			>
		</div>

		{#if aiError}
			<p class="status-line error">{aiError}</p>
		{/if}

		{#if displayOpen}
			<div class="display-panel">
				<div class="display-row">
					<span class="display-label">group</span>
					<button class="pill" class:active={groupMode === 'burner'} onclick={() => (groupMode = 'burner')}>Burner</button>
					<button class="pill" class:active={groupMode === 'date'} onclick={() => (groupMode = 'date')}>Date</button>
				</div>
				<div class="display-row">
					<span class="display-label">sort</span>
					{#each SORTS as s (s.field)}
						<button
							class="pill"
							class:active={activeSort?.field === s.field}
							onclick={() => setSort(s.field, s.defaultDir)}
						>
							{s.label}{activeSort?.field === s.field ? (activeSort.dir === 'asc' ? ' ↑' : ' ↓') : ''}
						</button>
					{/each}
					{#if sort}<button class="pill clear" onclick={() => (sort = undefined)}>default</button>{/if}
				</div>
				<div class="display-row">
					<span class="display-label">show</span>
					{#each ['open', 'all', 'done'] as const as st (st)}
						<button class="pill" class:active={status === st} onclick={() => (status = st)}>{st}</button>
					{/each}
				</div>
			</div>
		{/if}

		{#if toggleError}
			<p class="status-line error">{toggleError}</p>
		{/if}

		{#if loading}
			<p class="status-line">Loading…</p>
		{:else if loadError}
			<p class="status-line error">{loadError}</p>
		{:else if result.total === 0}
			<p class="empty">
				{todos.length === 0 ? 'Nothing on the stove. 🍳' : 'No tasks match — clear the search?'}
			</p>
		{:else}
			{#each result.groups as g (g.key)}
				<section class="group">
					<div class="group-head">
						<span class="dot c-{g.color}"></span>
						<span class="group-name">{g.label}</span>
						{#if g.hint}<span class="group-hint">{g.hint}</span>{/if}
						<span class="group-count">{g.openCount}</span>
					</div>
					<ul class="rows">
						{#each g.todos as todo (todo.note_id + ':' + todo.line)}
							<li class="row-item" class:done={todo.done} style="--depth: {todo.depth};">
								<button
									type="button"
									class="check c-{ringColor(todo)}"
									class:checked={todo.done}
									aria-label={todo.done ? 'Mark not done' : 'Mark done'}
									onclick={() => toggle(todo)}
								>
									{todo.done ? '✓' : ''}
								</button>
								<button type="button" class="task" onclick={() => openNote(todo)}>
									<span class="title">{todo.text_clean || '(empty task)'}</span>
									<span class="meta">
										{#if todo.due}<span class="m">{todo.due}</span>{/if}
										{#if todo.start}<span class="m">{todo.start}</span>{/if}
										{#if todo.pomodoros != null}<span class="m">{todo.pomodoros}p</span>{/if}
										{#if todo.tags.length}<span class="m">#{todo.tags.join(' #')}</span>{/if}
										{#if todo.locations.length}<span class="m">@{todo.locations.join(' @')}</span>{/if}
										{#if isSpoiling(todo, now)}<span class="m spoiling"
												>spoils · {ageInDays(todo.date, now)}d</span
											>{/if}
										<span class="src">{relativeDate(todo.date, now) || todo.note_id}</span>
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

	.todo-header {
		display: flex;
		align-items: center;
		justify-content: space-between;
		margin-bottom: var(--space-4);
	}
	.header-actions {
		display: flex;
		align-items: center;
		gap: var(--space-3);
	}
	.count {
		font-family: var(--font-term);
		font-size: var(--type-meta);
		color: var(--kv-dim);
	}

	/* Toolbar: one search bar + Ask + Display. */
	.toolbar {
		display: flex;
		align-items: center;
		gap: var(--space-2);
		margin-bottom: var(--space-3);
	}
	.search {
		flex: 1;
		min-width: 0;
	}
	.display-btn {
		flex: none;
		width: var(--tap-target);
		height: var(--tap-target);
		display: flex;
		align-items: center;
		justify-content: center;
		background: var(--surface-input);
		color: var(--kv-dim);
		border: 1px solid var(--border-default);
		border-radius: var(--radius-control);
		cursor: pointer;
		font-size: var(--type-body);
	}
	.display-btn.active,
	.display-btn:hover {
		color: var(--kv-accent);
		border-color: var(--border-accent);
	}

	/* Display popover. */
	.display-panel {
		display: flex;
		flex-direction: column;
		gap: var(--space-2);
		padding: var(--space-3);
		margin-bottom: var(--space-3);
		background: var(--surface-raised);
		border: 1px solid var(--border-default);
		border-radius: var(--radius-control);
	}
	.display-row {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: var(--space-2);
	}
	.display-label {
		font-family: var(--font-pixel);
		font-size: var(--type-label);
		letter-spacing: var(--tracking-pixel);
		text-transform: uppercase;
		color: var(--kv-dim);
		width: 3.5rem;
	}
	.pill {
		font-family: var(--font-term);
		font-size: var(--type-meta);
		color: var(--kv-dim);
		background: transparent;
		border: 1px solid var(--border-default);
		border-radius: var(--radius-control);
		padding: 2px 8px;
		cursor: pointer;
	}
	.pill:hover {
		color: var(--kv-ink);
	}
	.pill.active {
		color: var(--kv-accent);
		border-color: var(--border-accent);
	}
	.pill.clear {
		color: var(--kv-faint);
	}

	/* Quiet group header: colored dot + gray label + hint + count. */
	.group {
		margin-top: var(--space-5);
	}
	.group-head {
		display: flex;
		align-items: center;
		gap: var(--space-2);
		padding-bottom: var(--space-2);
		border-bottom: 1px solid var(--border-default);
	}
	.group-name {
		font-family: var(--font-term);
		font-size: var(--type-data);
		color: var(--kv-ink);
	}
	.group-hint {
		font-family: var(--font-term);
		font-size: var(--type-meta);
		color: var(--kv-faint);
	}
	.group-count {
		margin-left: auto;
		font-family: var(--font-term);
		font-size: var(--type-meta);
		color: var(--kv-dim);
	}

	/* Priority color lives ONLY here (dot + ring). */
	.c-danger {
		--c: var(--kv-danger);
	}
	.c-orange {
		--c: var(--kv-orange);
	}
	.c-accent {
		--c: var(--kv-accent);
	}
	.c-blue {
		--c: var(--kv-blue);
	}
	.c-dim {
		--c: var(--kv-dim);
	}
	.c-none {
		--c: var(--kv-faint);
	}
	.dot {
		width: 8px;
		height: 8px;
		border-radius: 50%;
		background: var(--c);
		flex: none;
	}

	.rows {
		list-style: none;
		margin: 0;
		padding: 0;
	}
	.row-item {
		display: flex;
		align-items: flex-start;
		gap: var(--space-3);
		padding: var(--space-3) 0;
		padding-left: calc(var(--depth, 0) * var(--space-4));
		border-bottom: 1px solid var(--kv-faint);
	}
	.row-item.done {
		opacity: 0.5;
	}

	/* Circular checkbox — the priority carrier. */
	.check {
		flex: none;
		width: 20px;
		height: 20px;
		margin-top: 1px;
		border-radius: 50%;
		border: 2px solid var(--c);
		background: color-mix(in srgb, var(--c) 12%, transparent);
		color: var(--kv-bg);
		font-size: 12px;
		line-height: 1;
		display: flex;
		align-items: center;
		justify-content: center;
		cursor: pointer;
	}
	.check.checked {
		background: var(--c);
	}

	.task {
		flex: 1;
		min-width: 0;
		display: flex;
		flex-direction: column;
		gap: 2px;
		background: none;
		border: none;
		padding: 0;
		text-align: left;
		cursor: pointer;
	}
	.title {
		font-family: var(--font-term);
		font-size: var(--type-data);
		color: var(--kv-ink);
	}
	.row-item.done .title {
		text-decoration: line-through;
	}

	/* One quiet meta line: neutral gray, color only for the spoiling cue. */
	.meta {
		display: flex;
		flex-wrap: wrap;
		align-items: baseline;
		gap: var(--space-2);
		font-family: var(--font-term);
		font-size: var(--type-meta);
	}
	.m {
		color: var(--kv-dim);
	}
	.m.spoiling {
		color: var(--kv-danger);
	}
	.src {
		margin-left: auto;
		color: var(--kv-faint);
		padding-left: var(--space-3);
	}

	.status-line {
		font-family: var(--font-term);
		font-size: var(--type-body);
		color: var(--kv-dim);
	}
	.status-line.error {
		color: var(--kv-danger);
	}
	.empty {
		font-family: var(--font-term);
		font-size: var(--type-body);
		color: var(--kv-dim);
		text-align: center;
		padding: var(--space-8) 0;
	}
</style>
