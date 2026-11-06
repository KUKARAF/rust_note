<script lang="ts">
	// Body-fat card for /stats: latest value, change vs the previous month, and a
	// monthly line chart of the `bodyfat` series (tenths of a percent in the daily
	// notes — see bodyfat.ts). Read-only; readings are entered on the /bodyfat tab.
	import { onMount } from 'svelte';
	import { resolve } from '$app/paths';
	import { apiGet, ApiError } from '$lib/api/client';
	import { BODYFAT_KEY, monthlyBodyFat } from './bodyfat';
	import type { StatsResponse } from './stats';

	interface Props {
		/** today (YYYY-MM-DD) */
		today: string;
	}
	let { today }: Props = $props();

	let months = $state<ReturnType<typeof monthlyBodyFat>>([]);
	let loaded = $state(false);
	let error = $state<string | null>(null);

	async function load() {
		try {
			const from = `${Number(today.slice(0, 4)) - 2}${today.slice(4)}`;
			const res = await apiGet<StatsResponse>(
				`/api/stats?metric=${BODYFAT_KEY}&from=${from}&to=${today}`
			);
			const s = res.series.find((x) => x.metric === BODYFAT_KEY);
			months = monthlyBodyFat(s?.days ?? []);
		} catch (err) {
			error =
				err instanceof ApiError && err.status === 0
					? "You're offline — body fat needs a connection."
					: err instanceof Error
						? err.message
						: 'Failed to load body fat';
		} finally {
			loaded = true;
		}
	}

	onMount(() => void load());

	// ---- monthly chart ----
	const W = 320;
	const H = 120;
	const PAD = { l: 30, r: 10, t: 10, b: 22 };
	const chart = $derived.by(() => {
		if (months.length === 0) return null;
		const vals = months.map((m) => m.value);
		const lo = Math.floor(Math.min(...vals) - 1);
		const hi = Math.ceil(Math.max(...vals) + 1);
		const x = (i: number) =>
			months.length === 1
				? (PAD.l + W - PAD.r) / 2
				: PAD.l + (i * (W - PAD.l - PAD.r)) / (months.length - 1);
		const y = (v: number) => PAD.t + ((hi - v) / (hi - lo)) * (H - PAD.t - PAD.b);
		return {
			lo,
			hi,
			pts: months.map((m, i) => ({ ...m, cx: x(i), cy: y(m.value) })),
			line: months
				.map((m, i) => `${i ? 'L' : 'M'}${x(i).toFixed(1)} ${y(m.value).toFixed(1)}`)
				.join(' ')
		};
	});
	const latest = $derived(months.length ? months[months.length - 1] : null);
	const delta = $derived(
		months.length >= 2 ? latest!.value - months[months.length - 2].value : null
	);
	const MONTH = [
		'Jan',
		'Feb',
		'Mar',
		'Apr',
		'May',
		'Jun',
		'Jul',
		'Aug',
		'Sep',
		'Oct',
		'Nov',
		'Dec'
	];
	const monthLabel = (m: string) => MONTH[Number(m.slice(5, 7)) - 1];
</script>

<div class="card bf">
	{#if latest}
		<div class="hero">
			<span class="big">{latest.value.toFixed(1)}<small>%</small></span>
			<span class="sub">
				{monthLabel(latest.month)} · last reading {latest.date}
				{#if delta !== null}
					<b class={delta <= 0 ? 'good' : 'up'}>{delta > 0 ? '+' : ''}{delta.toFixed(1)}</b> vs prev month
				{/if}
			</span>
		</div>
	{/if}

	{#if chart}
		<svg viewBox="0 0 {W} {H}" class="chart" role="img" aria-label="Monthly body fat percentage">
			{#each [chart.lo, (chart.lo + chart.hi) / 2, chart.hi] as t (t)}
				{@const ty = PAD.t + ((chart.hi - t) / (chart.hi - chart.lo)) * (H - PAD.t - PAD.b)}
				<line x1={PAD.l} x2={W - PAD.r} y1={ty} y2={ty} class="grid" />
				<text x={PAD.l - 4} y={ty + 3} class="axis" text-anchor="end">{t}</text>
			{/each}
			<path d={chart.line} fill="none" class="line" />
			{#each chart.pts as p (p.month)}
				<circle cx={p.cx} cy={p.cy} r="3.5" class="dot" />
				<text x={p.cx} y={H - 6} class="axis" text-anchor="middle">{monthLabel(p.month)}</text>
			{/each}
		</svg>
	{:else if error}
		<p class="err">{error}</p>
	{:else if loaded}
		<p class="empty">No readings yet.</p>
	{/if}

	<a class="log" href={resolve('/bodyfat')}>Log a measurement →</a>
</div>

<style>
	.bf {
		display: flex;
		flex-direction: column;
		gap: 14px;
	}
	.hero,
	.big {
		font-size: 34px;
		font-weight: 600;
		color: var(--ink);
		line-height: 1;
	}
	.big small {
		font-size: 16px;
		color: var(--muted);
		margin-left: 2px;
	}
	.sub {
		font-size: 12px;
		color: var(--muted);
	}
	.good {
		color: var(--good);
	}
	.up {
		color: var(--up);
	}
	.chart {
		width: 100%;
		height: auto;
	}
	.grid {
		stroke: var(--hair);
		stroke-width: 1;
	}
	.axis {
		fill: var(--muted);
		font-size: 9px;
	}
	.line {
		stroke: var(--sugar);
		stroke-width: 2;
	}
	.dot {
		fill: var(--sugar);
		stroke: var(--surface);
		stroke-width: 1.5;
	}
	.empty {
		margin: 0;
		color: var(--muted);
		font-size: 13px;
	}
	.err {
		margin: 0;
		font-size: 13px;
		color: var(--critical);
	}
	.log {
		align-self: flex-start;
		color: var(--ink-2);
		font-size: 13px;
		text-decoration: underline;
	}
</style>
