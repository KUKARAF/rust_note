<script lang="ts">
	// Recurring-todos manager, opened from the Todo board toolbar. Manages a
	// small list of recurring items (an emoji + label, either a `local` daily
	// frontmatter bool or a `foreign` source polled by URL) plus an integer
	// `order` that decides which emoji the public endpoint surfaces first.
	//
	// Backend contract (crates/server, same origin):
	//   GET  /api/recurring            -> { todos: RecurringTodo[] }  (incl. live `done`)
	//   PUT  /api/recurring            { todos: [...] (no done) } -> { todos: [...] }
	//   POST /api/recurring/{key}/done { done: boolean }          -> { ok: true }
	import { apiGet, apiPut, apiPost, ApiError } from '$lib/api/client';
	import Card from '$lib/design/Card.svelte';
	import Button from '$lib/design/Button.svelte';

	type Kind = 'local' | 'foreign' | 'calendar';

	interface RecurringTodo {
		key: string;
		label: string;
		emoji: string;
		order: number;
		kind: Kind;
		url: string | null;
		regex: string | null;
		done: boolean;
	}

	let {
		onclose
	}: {
		onclose: () => void;
	} = $props();

	let todos = $state<RecurringTodo[]>([]);
	let loadState = $state<'loading' | 'ok' | 'error'>('loading');
	let error = $state<string | null>(null);
	let saving = $state(false);
	let saved = $state(false);

	async function load() {
		loadState = 'loading';
		error = null;
		try {
			const res = await apiGet<{ todos: RecurringTodo[] }>('/api/recurring');
			todos = res.todos ?? [];
			loadState = 'ok';
		} catch (err) {
			loadState = 'error';
			error = err instanceof ApiError ? err.message : 'Failed to load recurring todos.';
		}
	}

	function slugify(text: string): string {
		return text
			.toLowerCase()
			.trim()
			.replace(/[^a-z0-9]+/g, '_')
			.replace(/^_+|_+$/g, '');
	}

	function addRow() {
		saved = false;
		todos = [
			...todos,
			{ key: '', label: '', emoji: '', order: 0, kind: 'local', url: null, regex: null, done: false }
		];
	}

	function removeRow(index: number) {
		saved = false;
		todos = todos.filter((_, i) => i !== index);
	}

	// 0→1→…→n→0, where n = number of rows. 0 means unranked/off.
	function cycleOrder(row: RecurringTodo) {
		saved = false;
		row.order = (row.order + 1) % (todos.length + 1);
	}

	function setKind(row: RecurringTodo, kind: Kind) {
		saved = false;
		row.kind = kind;
		if (kind === 'local') {
			row.url = null;
			row.regex = null;
		} else {
			// foreign & calendar both use a url
			if (row.url === null) row.url = '';
			// only calendar uses regex
			if (kind === 'calendar') {
				if (row.regex === null) row.regex = '';
			} else {
				row.regex = null;
			}
		}
	}

	async function toggleDone(row: RecurringTodo) {
		if (row.kind !== 'local' || row.key === '') return;
		const want = !row.done;
		row.done = want; // optimistic
		error = null;
		try {
			await apiPost(`/api/recurring/${encodeURIComponent(row.key)}/done`, { done: want });
		} catch (err) {
			row.done = !want; // revert
			error = err instanceof ApiError ? err.message : 'Failed to update done state.';
		}
	}

	/** Assign keys, drop `done`, validate uniqueness/non-emptiness. */
	function prepareForSave(): { todos: Omit<RecurringTodo, 'done'>[] } | null {
		const seen = new Set<string>();
		const out: Omit<RecurringTodo, 'done'>[] = [];
		for (const row of todos) {
			if (row.label.trim() === '') {
				error = 'Every recurring todo needs a label.';
				return null;
			}
			let key = row.key.trim() || slugify(row.label);
			if (key === '') key = `todo_${Math.random().toString(36).slice(2, 8)}`;
			// Ensure uniqueness by suffixing on collision.
			let unique = key;
			let n = 2;
			while (seen.has(unique)) unique = `${key}_${n++}`;
			seen.add(unique);
			// Persist the resolved key back so future toggles/saves are stable.
			row.key = unique;
			out.push({
				key: unique,
				label: row.label.trim(),
				emoji: row.emoji.trim(),
				order: row.order,
				kind: row.kind,
				// foreign & calendar send their url; local sends null.
				url: row.kind === 'local' ? null : (row.url?.trim() ?? ''),
				// only calendar sends a regex; others send null.
				regex: row.kind === 'calendar' ? (row.regex?.trim() ?? '') : null
			});
		}
		return { todos: out };
	}

	async function save() {
		error = null;
		saved = false;
		const payload = prepareForSave();
		if (payload === null) return;
		saving = true;
		try {
			const res = await apiPut<{ todos: RecurringTodo[] }>('/api/recurring', payload);
			todos = res.todos ?? [];
			saved = true;
		} catch (err) {
			error = err instanceof ApiError ? err.message : 'Failed to save recurring todos.';
		} finally {
			saving = false;
		}
	}

	function onKeydown(event: KeyboardEvent) {
		if (event.key === 'Escape') onclose();
	}

	function onBackdropClick(event: MouseEvent) {
		if (event.target === event.currentTarget) onclose();
	}

	load();
</script>

<svelte:window onkeydown={onKeydown} />

<div class="rt-backdrop" onclick={onBackdropClick} role="presentation">
	<div class="rt-wrap" role="dialog" aria-modal="true" aria-label="Recurring todos">
		<Card>
			<div class="rt">
				<div class="rt-header">
					<span class="rt-eyebrow">&gt; recurring todos</span>
					<button class="rt-close" onclick={onclose} aria-label="Close">✕</button>
				</div>

				<p class="rt-caption">0 = top priority; higher = lower priority. Smallest order among pending wins.</p>

				{#if loadState === 'loading'}
					<p class="rt-hint">loading…</p>
				{:else if loadState === 'error'}
					<p class="rt-error">{error}</p>
					<Button variant="outline" size="sm" onclick={load}>Retry</Button>
				{:else}
					{#if todos.length === 0}
						<p class="rt-hint">No recurring todos yet.</p>
					{/if}

					<ul class="rt-list">
						{#each todos as row, i (i)}
							<li class="rt-row">
								<div class="rt-row-main">
									<!-- Done: local = clickable toggle, foreign = read-only status dot. -->
									{#if row.kind === 'local'}
										<button
											type="button"
											class="rt-done"
											class:on={row.done}
											onclick={() => toggleDone(row)}
											disabled={row.key === ''}
											aria-label={row.done ? 'Mark not done' : 'Mark done'}
											title={row.key === '' ? 'Save first to toggle done' : 'Toggle done for today'}
										></button>
									{:else}
										{@const dotLabel = row.done
											? row.kind === 'calendar'
												? 'no matching event today'
												: 'satisfied'
											: row.kind === 'calendar'
												? 'matching event today'
												: 'pending'}
										<span
											class="rt-dot"
											class:on={row.done}
											title={dotLabel}
											aria-label={dotLabel}
										></span>
									{/if}

									<input
										class="rt-emoji"
										maxlength="3"
										placeholder="🔁"
										aria-label="Emoji"
										bind:value={row.emoji}
										oninput={() => (saved = false)}
									/>

									<input
										class="rt-label"
										placeholder="Label"
										aria-label="Label"
										bind:value={row.label}
										oninput={() => (saved = false)}
									/>

									<button
										type="button"
										class="rt-order"
										class:zero={row.order === 0}
										onclick={() => cycleOrder(row)}
										title="Click to change priority (0 = top priority)"
										aria-label="Priority order {row.order}"
									>
										{row.order}
									</button>

									<button
										type="button"
										class="rt-del"
										onclick={() => removeRow(i)}
										aria-label="Delete"
										title="Delete"
									>
										✕
									</button>
								</div>

								<div class="rt-row-kind">
									<div class="rt-kind">
										<button
											type="button"
											class="rt-kind-btn"
											class:sel={row.kind === 'local'}
											onclick={() => setKind(row, 'local')}
										>
											local
										</button>
										<button
											type="button"
											class="rt-kind-btn"
											class:sel={row.kind === 'foreign'}
											onclick={() => setKind(row, 'foreign')}
										>
											foreign
										</button>
										<button
											type="button"
											class="rt-kind-btn"
											class:sel={row.kind === 'calendar'}
											onclick={() => setKind(row, 'calendar')}
										>
											calendar
										</button>
									</div>

									{#if row.kind === 'foreign'}
										<input
											class="rt-url"
											placeholder="https://source…"
											aria-label="Source URL"
											bind:value={row.url}
											oninput={() => (saved = false)}
										/>
									{:else if row.kind === 'calendar'}
										<input
											class="rt-url"
											placeholder="https://…/calendar.ics"
											aria-label="iCal URL"
											bind:value={row.url}
											oninput={() => (saved = false)}
										/>
										<input
											class="rt-url"
											placeholder="regex"
											aria-label="Event title regex"
											bind:value={row.regex}
											oninput={() => (saved = false)}
										/>
										<span class="rt-keyhint">
											matches event titles (case-insensitive), e.g. <code>date night</code>
										</span>
									{:else}
										<span class="rt-keyhint">
											key: <code>{row.key || slugify(row.label) || '—'}</code>
										</span>
									{/if}
								</div>
							</li>
						{/each}
					</ul>

					<button type="button" class="rt-add" onclick={addRow}>+ Add recurring todo</button>

					{#if error}
						<p class="rt-error">{error}</p>
					{/if}

					<div class="rt-actions">
						<Button variant="primary" size="sm" onclick={save} disabled={saving}>
							{saving ? 'Saving…' : saved ? 'Saved ✓' : 'Save'}
						</Button>
						<Button variant="outline" size="sm" onclick={onclose}>Close</Button>
					</div>
				{/if}
			</div>
		</Card>
	</div>
</div>

<style>
	.rt-backdrop {
		position: fixed;
		inset: 0;
		background: rgba(3, 6, 3, 0.72);
		display: flex;
		align-items: flex-start;
		justify-content: center;
		padding: calc(var(--space-9) + var(--safe-top)) calc(var(--space-6) + var(--safe-right))
			calc(var(--space-9) + var(--safe-bottom)) calc(var(--space-6) + var(--safe-left));
		overflow-y: auto;
		z-index: 100;
	}

	.rt-wrap {
		width: 100%;
		max-width: 34rem;
	}

	.rt {
		display: flex;
		flex-direction: column;
		gap: var(--space-5);
	}

	.rt-header {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: var(--space-4);
	}

	.rt-eyebrow {
		font-family: var(--font-pixel);
		font-size: var(--type-label);
		letter-spacing: var(--tracking-pixel);
		text-transform: uppercase;
		color: var(--kv-accent);
	}

	.rt-close {
		flex: 0 0 auto;
		background: transparent;
		border: 1px solid var(--border-default);
		border-radius: var(--radius-control);
		color: var(--kv-dim);
		font-family: var(--font-pixel);
		font-size: var(--type-label);
		cursor: pointer;
		padding: 6px 8px;
	}

	.rt-close:hover {
		color: var(--kv-ink);
		border-color: var(--border-accent);
	}

	.rt-caption,
	.rt-hint {
		margin: 0;
		font-family: var(--font-term);
		font-size: var(--type-meta);
		color: var(--kv-dim);
	}

	.rt-error {
		margin: 0;
		font-family: var(--font-term);
		font-size: var(--type-meta);
		color: var(--kv-danger);
	}

	.rt-list {
		list-style: none;
		margin: 0;
		padding: 0;
		display: flex;
		flex-direction: column;
		gap: var(--space-3);
	}

	.rt-row {
		display: flex;
		flex-direction: column;
		gap: var(--space-3);
		background: var(--surface-input);
		border: 1px solid var(--border-default);
		border-radius: var(--radius-control);
		padding: var(--space-3) var(--space-4);
	}

	.rt-row-main {
		display: flex;
		align-items: center;
		gap: var(--space-3);
	}

	.rt-done {
		flex: 0 0 auto;
		width: 22px;
		height: 22px;
		border-radius: 50%;
		border: 2px solid var(--border-accent);
		background: transparent;
		cursor: pointer;
		padding: 0;
	}

	.rt-done.on {
		background: var(--kv-accent);
		border-color: var(--kv-accent);
	}

	.rt-done:disabled {
		opacity: 0.4;
		cursor: not-allowed;
	}

	.rt-dot {
		flex: 0 0 auto;
		width: 12px;
		height: 12px;
		margin: 0 5px;
		border-radius: 50%;
		background: var(--kv-orange);
	}

	.rt-dot.on {
		background: var(--kv-accent);
	}

	.rt-emoji {
		flex: 0 0 auto;
		width: 44px;
		text-align: center;
		background: var(--surface-card);
		border: 1px solid var(--border-default);
		border-radius: var(--radius-control);
		padding: 7px 4px;
		font-family: var(--font-term);
		font-size: var(--type-body);
		color: var(--kv-ink);
	}

	.rt-label {
		flex: 1;
		min-width: 0;
		background: var(--surface-card);
		border: 1px solid var(--border-default);
		border-radius: var(--radius-control);
		padding: 7px 9px;
		font-family: var(--font-term);
		font-size: var(--type-body);
		color: var(--kv-ink);
	}

	.rt-emoji:focus,
	.rt-label:focus,
	.rt-url:focus {
		outline: none;
		border-color: var(--kv-accent);
	}

	.rt-order {
		flex: 0 0 auto;
		width: 32px;
		height: 32px;
		border-radius: var(--radius-control);
		border: 1px solid var(--border-accent);
		background: transparent;
		color: var(--kv-accent);
		font-family: var(--font-pixel);
		font-size: var(--type-label);
		cursor: pointer;
	}

	.rt-order.zero {
		border-color: var(--border-default);
		color: var(--kv-dim);
	}

	.rt-del {
		flex: 0 0 auto;
		width: 26px;
		height: 26px;
		background: transparent;
		border: 1px solid var(--border-default);
		border-radius: var(--radius-control);
		color: var(--kv-dim);
		cursor: pointer;
		font-size: 12px;
		line-height: 1;
	}

	.rt-del:hover {
		color: var(--kv-danger);
		border-color: var(--kv-danger);
	}

	.rt-row-kind {
		display: flex;
		align-items: center;
		gap: var(--space-3);
		flex-wrap: wrap;
	}

	.rt-kind {
		display: inline-flex;
		border: 1px solid var(--border-default);
		border-radius: var(--radius-control);
		overflow: hidden;
		flex: 0 0 auto;
	}

	.rt-kind-btn {
		background: transparent;
		border: none;
		color: var(--kv-dim);
		font-family: var(--font-pixel);
		font-size: var(--type-chip);
		text-transform: uppercase;
		letter-spacing: var(--tracking-pixel);
		padding: 6px 10px;
		cursor: pointer;
	}

	.rt-kind-btn.sel {
		background: color-mix(in srgb, var(--kv-accent) 14%, transparent);
		color: var(--kv-accent);
	}

	.rt-url {
		flex: 1;
		min-width: 8rem;
		background: var(--surface-card);
		border: 1px solid var(--border-default);
		border-radius: var(--radius-control);
		padding: 7px 9px;
		font-family: var(--font-term);
		font-size: var(--type-meta);
		color: var(--kv-ink);
	}

	.rt-keyhint {
		font-family: var(--font-term);
		font-size: var(--type-meta);
		color: var(--kv-dim);
	}

	.rt-keyhint code {
		font-family: var(--font-term);
		color: var(--kv-faint);
	}

	.rt-add {
		align-self: flex-start;
		background: transparent;
		border: 1px dashed var(--border-accent);
		border-radius: var(--radius-control);
		color: var(--kv-accent);
		font-family: var(--font-pixel);
		font-size: var(--type-label);
		text-transform: uppercase;
		letter-spacing: var(--tracking-pixel);
		padding: 8px 12px;
		cursor: pointer;
	}

	.rt-add:hover {
		opacity: 0.82;
	}

	.rt-actions {
		display: flex;
		gap: var(--space-4);
	}
</style>
