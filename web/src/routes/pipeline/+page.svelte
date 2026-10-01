<script lang="ts">
	// Pipeline tracker — leads + applications, "whose court is the ball in".
	// Needs-attention first (ball in play AND overdue), then open items by stage.
	// Reads come from GET /api/pipeline.
	import { onMount } from 'svelte';
	import { goto } from '$app/navigation';
	import { resolve } from '$app/paths';
	import { ApiError } from '$lib/api/client';
	import { flagLoginRequired } from '$lib/stores/auth';
	import { encodeNotePath } from '$lib/notes/path';
	import Card from '$lib/design/Card.svelte';
	import Button from '$lib/design/Button.svelte';
	import Chip from '$lib/design/Chip.svelte';
	import Checkbox from '$lib/design/Checkbox.svelte';
	import SectionTitle from '$lib/design/SectionTitle.svelte';
	import {
		fetchPipeline,
		needsAttention,
		groupByStage,
		isClosed,
		comparePipeline,
		relativeExpected,
		type PipelineItem,
		type KindFilter
	} from '$lib/notes/pipeline';

	const now = new Date();

	let items = $state<PipelineItem[]>([]);
	let loading = $state(true);
	let loadError = $state<string | null>(null);
	let kind = $state<KindFilter>('all');
	let showClosed = $state(false);

	const KINDS: { value: KindFilter; label: string }[] = [
		{ value: 'all', label: 'All' },
		{ value: 'lead', label: 'Leads' },
		{ value: 'application', label: 'Applications' }
	];

	const visible = $derived(kind === 'all' ? items : items.filter((i) => i.kind === kind));
	const attention = $derived(needsAttention(visible.filter((i) => !isClosed(i))));
	const attentionIds = $derived(new Set(attention.map((i) => i.id)));
	const open = $derived(visible.filter((i) => !isClosed(i) && !attentionIds.has(i.id)));
	const groups = $derived(groupByStage(open));
	const closed = $derived(visible.filter(isClosed).sort(comparePipeline));
	const openTotal = $derived(attention.length + open.length);

	async function load() {
		loading = true;
		loadError = null;
		try {
			items = await fetchPipeline('all', showClosed);
		} catch (err) {
			if (err instanceof ApiError && err.status === 401) {
				flagLoginRequired();
				await goto(resolve('/login'));
				return;
			}
			if (err instanceof ApiError && err.status === 0) {
				loadError = "You're offline — the pipeline needs a connection to aggregate notes.";
				return;
			}
			loadError = err instanceof Error ? err.message : 'Failed to load pipeline';
		} finally {
			loading = false;
		}
	}

	onMount(() => {
		void load();
	});

	function toggleClosed() {
		void load();
	}

	function openNote(item: PipelineItem) {
		void goto(resolve(`/notes/${encodeNotePath(item.id)}`));
	}
</script>

{#snippet card(item: PipelineItem)}
	<li class="row-item" class:closed={isClosed(item)}>
		<button type="button" class="item" onclick={() => openNote(item)}>
			<span class="line">
				<span class="ball ball-{item.ball}" title="ball: {item.ball}"></span>
				<span class="title">{item.company}</span>
				{#if item.role}<span class="role">· {item.role}</span>{/if}
				<span class="chips">
					<Chip color="dim" variant="outline">{item.kind}</Chip>
					{#if item.stage}<Chip color={item.ball === 'ours' ? 'accent' : 'dim'}>{item.stage}</Chip
						>{/if}
					{#if item.ball === 'ours'}<Chip color="accent">your move</Chip>{/if}
				</span>
			</span>
			<span class="meta">
				{#if item.next_action}<span class="m action">{item.next_action}</span>{/if}
				{#if item.priority != null}<span class="m">p{item.priority}</span>{/if}
				{#if item.closed_reason}<span class="m">{item.closed_reason}</span>{/if}
				{#if item.expected_at}
					<span class="when" class:overdue={item.overdue && item.ball !== 'none'}>
						{relativeExpected(item.expected_at, now)}
					</span>
				{/if}
			</span>
		</button>
	</li>
{/snippet}

<div class="pipeline-page">
	<Card class="pipeline-card">
		<div class="pipeline-header">
			<SectionTitle>Pipeline</SectionTitle>
			<div class="header-actions">
				<span class="count">{openTotal} open</span>
				<Button variant="outline" size="sm" onclick={load}>Refresh</Button>
			</div>
		</div>

		<div class="toolbar">
			{#each KINDS as k (k.value)}
				<button class="pill" class:active={kind === k.value} onclick={() => (kind = k.value)}>
					{k.label}
				</button>
			{/each}
			<span class="spacer"></span>
			<span onchange={toggleClosed} role="presentation">
				<Checkbox bind:checked={showClosed} label="Show closed" id="pipeline-show-closed" />
			</span>
		</div>

		{#if loading}
			<p class="status-line">Loading…</p>
		{:else if loadError}
			<p class="status-line error">{loadError}</p>
		{:else if visible.length === 0}
			<p class="empty">Nothing in the pipeline yet.</p>
		{:else}
			{#if attention.length > 0}
				<section class="group">
					<div class="group-head">
						<span class="dot attention"></span>
						<span class="group-name">Needs attention</span>
						<span class="group-hint">ball in play, past due</span>
						<span class="group-count">{attention.length}</span>
					</div>
					<ul class="rows">
						{#each attention as item (item.id)}{@render card(item)}{/each}
					</ul>
				</section>
			{/if}

			<section class="group">
				<div class="group-head">
					<span class="group-name section">Pipeline</span>
					<span class="group-count">{open.length}</span>
				</div>
				{#if groups.length === 0}
					<p class="empty small">All caught up.</p>
				{/if}
				{#each groups as g (g.stage)}
					<div class="stage-head">
						<span class="stage-name">{g.stage}</span>
						<span class="group-count">{g.items.length}</span>
					</div>
					<ul class="rows">
						{#each g.items as item (item.id)}{@render card(item)}{/each}
					</ul>
				{/each}
			</section>

			{#if showClosed && closed.length > 0}
				<section class="group">
					<div class="group-head">
						<span class="group-name section">Closed</span>
						<span class="group-count">{closed.length}</span>
					</div>
					<ul class="rows">
						{#each closed as item (item.id)}{@render card(item)}{/each}
					</ul>
				</section>
			{/if}
		{/if}
	</Card>
</div>

<style>
	.pipeline-page {
		max-width: 46rem;
		margin: 0 auto;
	}
	.pipeline-header {
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
	.toolbar {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: var(--space-2);
		margin-bottom: var(--space-3);
	}
	.spacer {
		flex: 1;
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

	.group {
		margin-top: var(--space-5);
	}
	.group-head,
	.stage-head {
		display: flex;
		align-items: center;
		gap: var(--space-2);
		padding-bottom: var(--space-2);
		border-bottom: 1px solid var(--border-default);
	}
	.stage-head {
		margin-top: var(--space-4);
	}
	.group-name {
		font-family: var(--font-term);
		font-size: var(--type-data);
		color: var(--kv-ink);
	}
	.stage-name {
		font-family: var(--font-pixel);
		font-size: var(--type-label);
		letter-spacing: var(--tracking-pixel);
		text-transform: uppercase;
		color: var(--kv-dim);
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
	.dot {
		width: 8px;
		height: 8px;
		border-radius: 50%;
		flex: none;
	}
	.dot.attention {
		background: var(--kv-danger);
	}

	.rows {
		list-style: none;
		margin: 0;
		padding: 0;
	}
	.row-item {
		border-bottom: 1px solid var(--kv-faint);
	}
	.row-item.closed {
		opacity: 0.5;
	}
	.item {
		width: 100%;
		display: flex;
		flex-direction: column;
		gap: 2px;
		padding: var(--space-3) 0;
		background: none;
		border: none;
		text-align: left;
		cursor: pointer;
	}
	.line {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: var(--space-2);
	}
	.title {
		font-family: var(--font-term);
		font-size: var(--type-data);
		color: var(--kv-ink);
	}
	.role {
		font-family: var(--font-term);
		font-size: var(--type-data);
		color: var(--kv-dim);
	}
	.chips {
		display: inline-flex;
		flex-wrap: wrap;
		gap: var(--space-2);
		margin-left: auto;
	}
	/* Ball indicator: ours = accent ("needs you"), theirs = muted, none = faint. */
	.ball {
		width: 8px;
		height: 8px;
		border-radius: 50%;
		flex: none;
	}
	.ball-ours {
		background: var(--kv-accent);
	}
	.ball-theirs {
		border: 1px solid var(--kv-dim);
	}
	.ball-none {
		border: 1px solid var(--kv-faint);
	}
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
	.when {
		margin-left: auto;
		color: var(--kv-dim);
	}
	.when.overdue {
		color: var(--kv-danger);
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
	.empty.small {
		padding: var(--space-4) 0;
	}
</style>
