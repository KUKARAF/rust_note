<script lang="ts">
	// Stats board: read-only charts of daily-note frontmatter metrics, plus a
	// small panel to define metrics (units/label/chart/agg). Data is entered via
	// the API or manual text; this view mainly displays it. Client-side fetch on
	// mount (global ssr=false), same pattern as /todo.
	import { onMount } from 'svelte';
	import { goto } from '$app/navigation';
	import { resolve } from '$app/paths';
	import { apiGet, apiPut, apiDelete, ApiError } from '$lib/api/client';
	import { flagLoginRequired } from '$lib/stores/auth';
	import Card from '$lib/design/Card.svelte';
	import Button from '$lib/design/Button.svelte';
	import Input from '$lib/design/Input.svelte';
	import SectionTitle from '$lib/design/SectionTitle.svelte';
	import {
		valueRange,
		statsRangeQuery,
		CHART_KINDS,
		AGG_KINDS,
		type Series,
		type MetricDef,
		type StatsResponse,
		type DayValue
	} from '$lib/stats/stats';

	let series = $state<Series[]>([]);
	let registry = $state<MetricDef[]>([]);
	let loading = $state(true);
	let loadError = $state<string | null>(null);
	let saveError = $state<string | null>(null);

	// Date range (empty = server default, ~last 30 days).
	let from = $state('');
	let to = $state('');

	// Add/edit metric form.
	let fMetric = $state('');
	let fLabel = $state('');
	let fUnit = $state('');
	let fChart = $state('line');
	let fAgg = $state('sum');
	let saving = $state(false);

	async function loadAll() {
		loading = true;
		loadError = null;
		try {
			const [stats, reg] = await Promise.all([
				apiGet<StatsResponse>(`/api/stats${statsRangeQuery(from, to)}`),
				apiGet<MetricDef[]>('/api/stats/registry')
			]);
			series = stats.series;
			registry = reg;
		} catch (err) {
			if (err instanceof ApiError && err.status === 401) {
				flagLoginRequired();
				await goto(resolve('/login'));
				return;
			}
			if (err instanceof ApiError && err.status === 0) {
				loadError = "You're offline — the stats board needs a connection.";
				return;
			}
			loadError = err instanceof Error ? err.message : 'Failed to load stats';
		} finally {
			loading = false;
		}
	}

	onMount(() => {
		void loadAll();
	});

	function editMetric(def: MetricDef) {
		fMetric = def.metric;
		fLabel = def.label;
		fUnit = def.unit;
		fChart = def.chart;
		fAgg = def.agg;
		saveError = null;
	}

	function resetForm() {
		fMetric = '';
		fLabel = '';
		fUnit = '';
		fChart = 'line';
		fAgg = 'sum';
		saveError = null;
	}

	async function saveMetric() {
		const metric = fMetric.trim();
		if (metric === '') {
			saveError = 'Metric name is required.';
			return;
		}
		saving = true;
		saveError = null;
		try {
			await apiPut<MetricDef>(`/api/stats/registry/${encodeURIComponent(metric)}`, {
				label: fLabel.trim(),
				unit: fUnit.trim(),
				chart: fChart,
				agg: fAgg
			});
			resetForm();
			await loadAll();
		} catch (err) {
			saveError = err instanceof Error ? err.message : 'Failed to save metric';
		} finally {
			saving = false;
		}
	}

	async function removeMetric(metric: string) {
		saving = true;
		saveError = null;
		try {
			await apiDelete(`/api/stats/registry/${encodeURIComponent(metric)}`);
			await loadAll();
		} catch (err) {
			saveError = err instanceof Error ? err.message : 'Failed to delete metric';
		} finally {
			saving = false;
		}
	}

	// ---- chart geometry (inline SVG, no deps) ----
	const W = 300;
	const H = 48;

	function xAt(i: number, n: number): number {
		return n <= 1 ? W / 2 : (i / (n - 1)) * W;
	}

	function linePath(days: DayValue[]): string {
		if (days.length === 0) return '';
		const { min, max } = valueRange(days);
		const span = max - min || 1;
		return days
			.map((d, i) => {
				const x = xAt(i, days.length);
				const y = H - ((d.value - min) / span) * H;
				return `${i === 0 ? 'M' : 'L'}${x.toFixed(1)},${y.toFixed(1)}`;
			})
			.join(' ');
	}

	interface Bar {
		/** Source day, used as the `{#each}` key. */
		date: string;
		x: number;
		y: number;
		w: number;
		h: number;
	}
	function bars(days: DayValue[]): Bar[] {
		if (days.length === 0) return [];
		const { max } = valueRange(days);
		const span = max || 1;
		const bw = Math.max(2, W / days.length - 2);
		return days.map((d, i) => {
			const h = Math.max(1, (d.value / span) * H);
			return { date: d.date, x: xAt(i, days.length) - bw / 2, y: H - h, w: bw, h };
		});
	}

	function total(days: DayValue[]): number {
		return days.reduce((s, d) => s + d.value, 0);
	}
</script>

<div class="stats-page">
	<Card class="stats-card">
		<div class="stats-header">
			<SectionTitle>Stats</SectionTitle>
			<div class="header-actions">
				<Button variant="outline" size="sm" onclick={loadAll}>Refresh</Button>
			</div>
		</div>

		<div class="range">
			<label class="range-field">
				<span>from</span>
				<Input type="date" bind:value={from} />
			</label>
			<label class="range-field">
				<span>to</span>
				<Input type="date" bind:value={to} />
			</label>
			<Button variant="outline" size="sm" onclick={loadAll}>Apply</Button>
		</div>

		{#if loading}
			<p class="status-line">Loading…</p>
		{:else if loadError}
			<p class="status-line error">{loadError}</p>
		{:else if series.length === 0}
			<p class="status-line">
				No charted metrics yet. Add a metric below, then log values via the API or by editing a
				daily note's frontmatter (e.g. <code>caffeine: 40@0720</code>).
			</p>
		{:else}
			<div class="charts">
				{#each series as s (s.metric)}
					<section class="metric">
						<div class="metric-head">
							<span class="metric-label">{s.label}</span>
							<span class="metric-meta">
								{#if s.chart === 'boolean'}
									{s.days.length} day{s.days.length === 1 ? '' : 's'}
								{:else}
									{total(s.days)}{s.unit ? ` ${s.unit}` : ''} total · {s.agg}
								{/if}
							</span>
						</div>

						{#if s.days.length === 0}
							<p class="status-line dim">no data in range</p>
						{:else if s.chart === 'boolean' || s.chart === 'heatmap'}
							<div class="cells">
								{#each s.days as d (d.date)}
									<span class="cell on" title={d.date}></span>
								{/each}
							</div>
						{:else if s.chart === 'bar'}
							<svg
								class="chart"
								viewBox="0 0 {W} {H}"
								preserveAspectRatio="none"
								role="img"
								aria-label={`${s.label} bar chart`}
							>
								{#each bars(s.days) as b (b.date)}
									<rect x={b.x} y={b.y} width={b.w} height={b.h} class="bar" />
								{/each}
							</svg>
						{:else}
							<svg
								class="chart"
								viewBox="0 0 {W} {H}"
								preserveAspectRatio="none"
								role="img"
								aria-label={`${s.label} line chart`}
							>
								<path d={linePath(s.days)} class="line" fill="none" />
							</svg>
						{/if}
					</section>
				{/each}
			</div>
		{/if}

		<!-- Metric registry management -->
		<div class="registry">
			<SectionTitle>Metrics</SectionTitle>
			{#if registry.length > 0}
				<ul class="metric-list">
					{#each registry as def (def.metric)}
						<li class="metric-row">
							<button type="button" class="metric-name" onclick={() => editMetric(def)}>
								{def.metric}
							</button>
							<span class="metric-tags">
								{def.chart}{def.unit ? ` · ${def.unit}` : ''} · {def.agg}
							</span>
							<button
								type="button"
								class="del"
								disabled={saving}
								onclick={() => removeMetric(def.metric)}
								title="delete definition">✕</button
							>
						</li>
					{/each}
				</ul>
			{/if}

			<div class="metric-form">
				<Input
					type="text"
					placeholder="metric (e.g. caffeine or exercise.cardio)"
					bind:value={fMetric}
				/>
				<Input type="text" placeholder="label" bind:value={fLabel} />
				<Input type="text" placeholder="unit (mg, g, min…)" bind:value={fUnit} />
				<label class="sel">
					chart
					<select bind:value={fChart}>
						{#each CHART_KINDS as c (c)}<option value={c}>{c}</option>{/each}
					</select>
				</label>
				<label class="sel">
					agg
					<select bind:value={fAgg}>
						{#each AGG_KINDS as a (a)}<option value={a}>{a}</option>{/each}
					</select>
				</label>
				<Button variant="primary" size="sm" onclick={saveMetric} disabled={saving}>
					{saving ? 'Saving…' : 'Save metric'}
				</Button>
			</div>
			{#if saveError}
				<p class="status-line error">{saveError}</p>
			{/if}
		</div>
	</Card>
</div>

<style>
	.stats-page {
		max-width: 46rem;
		margin: 0 auto;
	}
	.stats-header,
	.registry :global(.section-title) {
		margin-bottom: var(--space-3);
	}
	.stats-header {
		display: flex;
		align-items: center;
		justify-content: space-between;
	}
	.range {
		display: flex;
		align-items: flex-end;
		gap: var(--space-3);
		margin-bottom: var(--space-4);
		flex-wrap: wrap;
	}
	.range-field {
		display: flex;
		flex-direction: column;
		gap: var(--space-1);
		font-family: var(--font-term);
		font-size: var(--type-meta);
		color: var(--kv-dim);
	}
	.charts {
		display: flex;
		flex-direction: column;
		gap: var(--space-5);
	}
	.metric-head {
		display: flex;
		justify-content: space-between;
		align-items: baseline;
		margin-bottom: var(--space-2);
	}
	.metric-label {
		font-family: var(--font-term);
		font-size: var(--type-data);
		color: var(--kv-ink);
	}
	.metric-meta {
		font-family: var(--font-term);
		font-size: var(--type-meta);
		color: var(--kv-dim);
	}
	.chart {
		width: 100%;
		height: 60px;
		display: block;
	}
	.line {
		stroke: var(--kv-accent);
		stroke-width: 1.5;
		vector-effect: non-scaling-stroke;
	}
	.bar {
		fill: var(--kv-accent);
	}
	.cells {
		display: flex;
		flex-wrap: wrap;
		gap: 3px;
	}
	.cell {
		width: 12px;
		height: 12px;
		border-radius: var(--radius-control);
		background: var(--kv-faint);
	}
	.cell.on {
		background: var(--kv-accent);
	}
	.registry {
		margin-top: var(--space-6);
		border-top: 1px solid var(--border-default);
		padding-top: var(--space-4);
	}
	.metric-list {
		list-style: none;
		margin: 0 0 var(--space-3);
		padding: 0;
		display: flex;
		flex-direction: column;
		gap: var(--space-1);
	}
	.metric-row {
		display: flex;
		align-items: center;
		gap: var(--space-2);
		font-family: var(--font-term);
		font-size: var(--type-meta);
	}
	.metric-name {
		background: none;
		border: none;
		color: var(--kv-accent);
		cursor: pointer;
		font: inherit;
		padding: 0;
	}
	.metric-tags {
		color: var(--kv-dim);
		flex: 1;
	}
	.del {
		background: none;
		border: none;
		color: var(--kv-danger);
		cursor: pointer;
		font: inherit;
	}
	.metric-form {
		display: flex;
		flex-wrap: wrap;
		gap: var(--space-2);
		align-items: center;
	}
	.sel {
		display: flex;
		align-items: center;
		gap: var(--space-1);
		font-family: var(--font-term);
		font-size: var(--type-meta);
		color: var(--kv-dim);
	}
	.sel select {
		background: var(--surface-input);
		color: var(--kv-ink);
		border: 1px solid var(--border-default);
		border-radius: var(--radius-control);
		font: inherit;
		padding: 2px 4px;
	}
	.status-line {
		font-family: var(--font-term);
		font-size: var(--type-body);
		color: var(--kv-dim);
	}
	.status-line.dim {
		color: var(--kv-faint);
		font-size: var(--type-meta);
	}
	.status-line.error {
		color: var(--kv-danger);
	}
	code {
		font-family: var(--font-term);
		color: var(--kv-accent);
	}
</style>
