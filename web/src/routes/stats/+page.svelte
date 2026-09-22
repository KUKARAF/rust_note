<script lang="ts">
	// Stats board — MINIMAL / GLANCEABLE / MOBILE-FIRST (concept C).
	// Reading order: Today → Substances → Food calendar. Single vertical column,
	// large tap targets, a sticky top bar with a light/dark toggle. All visuals
	// are self-contained under `.stats-app` (its own palette, flipped per theme)
	// rather than the site's CRT tokens, matching the picked UX direction.
	// Client-side fetch on mount (global ssr=false), same pattern as /todo.
	import { onMount } from 'svelte';
	import { goto } from '$app/navigation';
	import { resolve } from '$app/paths';
	import { apiGet, apiPut, ApiError } from '$lib/api/client';
	import { flagLoginRequired } from '$lib/stores/auth';
	import FoodCalendar from '$lib/stats/FoodCalendar.svelte';
	import {
		dayValueMap,
		dayPointsMap,
		foodCategory,
		sugarLevel,
		lastNDates,
		sparkline,
		parseISODate,
		toISODate,
		addDays,
		FOOD_REGISTRY_DEFAULTS,
		type Series,
		type MetricDef,
		type StatsResponse,
		type StatPoint,
		type FoodCategory,
		type FoodDay
	} from '$lib/stats/stats';

	let series = $state<Series[]>([]);
	let loading = $state(true);
	let loadError = $state<string | null>(null);

	const today = new Date().toLocaleDateString('en-CA'); // YYYY-MM-DD, local day

	// Theme: default to the OS preference, then let the toggle override it.
	function prefersDark(): boolean {
		return (
			typeof window !== 'undefined' && !!window.matchMedia?.('(prefers-color-scheme: dark)').matches
		);
	}
	let dark = $state(prefersDark());

	/**
	 * Ensure the food/substance metrics exist in the registry so they show up in
	 * `GET /api/stats`. Idempotent and non-clobbering: only metrics absent from
	 * `existing` are PUT. Returns true if anything was added.
	 */
	async function ensureFoodRegistry(existing: MetricDef[]): Promise<boolean> {
		const have = new Set(existing.map((d) => d.metric));
		const missing = FOOD_REGISTRY_DEFAULTS.filter((d) => !have.has(d.metric));
		if (missing.length === 0) return false;
		await Promise.all(
			missing.map((d) =>
				apiPut<MetricDef>(`/api/stats/registry/${encodeURIComponent(d.metric)}`, {
					label: d.label,
					unit: d.unit,
					chart: d.chart,
					agg: d.agg
				})
			)
		);
		return true;
	}

	async function loadAll() {
		loading = true;
		loadError = null;
		try {
			// Make sure the food/substance metrics are registered (idempotent) so
			// they come back in GET /api/stats; then read a wide-enough window.
			const reg = await apiGet<MetricDef[]>('/api/stats/registry');
			await ensureFoodRegistry(reg);
			const from = toISODate(addDays(parseISODate(today) ?? new Date(), -48));
			const stats = await apiGet<StatsResponse>(`/api/stats?from=${from}&to=${today}`);
			series = stats.series;
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

	// ---- derived data --------------------------------------------------------

	function seriesBy(metric: string): Series | undefined {
		return series.find((s) => s.metric === metric);
	}

	const caffeineMap = $derived(dayValueMap(seriesBy('caffeine')));
	const alcoholMap = $derived(dayValueMap(seriesBy('alcohol')));
	const sugarMap = $derived(dayValueMap(seriesBy('sugar')));
	const veganMap = $derived(dayValueMap(seriesBy('vegan')));
	const vegetarianMap = $derived(dayValueMap(seriesBy('vegetarian')));
	const caffeinePts = $derived(dayPointsMap(seriesBy('caffeine')));
	const alcoholPts = $derived(dayPointsMap(seriesBy('alcohol')));
	const sugarPts = $derived(dayPointsMap(seriesBy('sugar')));

	function ptsFor(key: string): Map<string, StatPoint[]> {
		return key === 'caffeine' ? caffeinePts : key === 'alcohol' ? alcoholPts : sugarPts;
	}
	function mapFor(key: string): Map<string, number> {
		return key === 'caffeine' ? caffeineMap : key === 'alcohol' ? alcoholMap : sugarMap;
	}

	function foodDayFor(date: string): FoodDay {
		const category = foodCategory(veganMap.get(date), vegetarianMap.get(date));
		return {
			date,
			category,
			logged: category !== 'none',
			sugar: sugarMap.get(date) ?? 0,
			caffeine: caffeineMap.get(date) ?? 0,
			alcohol: alcoholMap.get(date) ?? 0,
			caffeineCount: (caffeinePts.get(date) ?? []).length,
			alcoholCount: (alcoholPts.get(date) ?? []).length
		};
	}

	// Calendar range: the fetched window (last ~7 weeks) through today.
	const foodDays = $derived.by<FoodDay[]>(() => {
		const end = parseISODate(today) ?? new Date();
		const start = addDays(end, -48);
		const out: FoodDay[] = [];
		for (let d = start; d <= end; d = addDays(d, 1)) out.push(foodDayFor(toISODate(d)));
		return out;
	});

	const todayFd = $derived(foodDayFor(today));
	const last7 = $derived(lastNDates(today, 7));
	const veganCt = $derived(last7.filter((d) => veganMap.get(d) === 1).length);

	// Substance rows (compact value + 7-day sparkline + delta, tap to expand).
	interface SubDef {
		key: 'caffeine' | 'alcohol' | 'sugar';
		name: string;
		sub: string;
		unit: string;
		color: string;
	}
	const SUB_DEFS: SubDef[] = [
		{
			key: 'caffeine',
			name: 'Caffeine',
			sub: 'vs ~200 mg cap',
			unit: 'mg',
			color: 'var(--caffeine)'
		},
		{ key: 'alcohol', name: 'Alcohol', sub: 'aim for 0', unit: 'g', color: 'var(--alcohol)' },
		{ key: 'sugar', name: 'Sugar', sub: 'vs ~40 g cap', unit: 'g', color: 'var(--sugar)' }
	];

	interface SubRow {
		def: SubDef;
		vals: number[];
		now: number;
		delta: number;
		deltaCls: '' | 'up' | 'dn';
		deltaTxt: string;
		spark: ReturnType<typeof sparkline>;
		points: StatPoint[];
		pointsMax: number;
	}
	const subRows = $derived.by<SubRow[]>(() =>
		SUB_DEFS.map((def) => {
			const map = mapFor(def.key);
			const vals = last7.map((d) => map.get(d) ?? 0);
			const now = vals[vals.length - 1] ?? 0;
			const prev = vals[vals.length - 2] ?? 0;
			const delta = now - prev;
			// Less is better for all three, so a rise reads "up" (worse).
			const deltaCls = delta === 0 ? '' : delta > 0 ? 'up' : 'dn';
			const deltaTxt = delta === 0 ? '±0' : `${delta > 0 ? '+' : '−'}${Math.abs(delta)}`;
			const points = ptsFor(def.key).get(today) ?? [];
			const pointsMax = Math.max(1, ...points.map((p) => p.value));
			return {
				def,
				vals,
				now,
				delta,
				deltaCls,
				deltaTxt,
				spark: sparkline(vals, 92, 30),
				points,
				pointsMax
			};
		})
	);

	let expanded = $state<Record<string, boolean>>({});
	function toggleRow(key: string) {
		expanded = { ...expanded, [key]: !expanded[key] };
	}

	// ---- formatting ----------------------------------------------------------
	const WD = ['Sunday', 'Monday', 'Tuesday', 'Wednesday', 'Thursday', 'Friday', 'Saturday'];
	const MON = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec'];
	const CAT_LABEL: Record<FoodCategory, string> = {
		vegan: 'Vegan',
		vegetarian: 'Vegetarian',
		meat: 'Meat',
		none: 'No food'
	};
	const CAT_STEM: Record<'vegan' | 'vegetarian' | 'meat', string> = {
		vegan: 'vegan',
		vegetarian: 'veg',
		meat: 'meat'
	};
	function catVar(cat: 'vegan' | 'vegetarian' | 'meat', lvl: number): string {
		return `var(--${CAT_STEM[cat]}-${lvl})`;
	}
	function fmt(n: number): string {
		return n.toLocaleString('en-US');
	}
	const todayLabel = $derived.by(() => {
		const d = parseISODate(today);
		if (!d) return today;
		return { wd: WD[d.getUTCDay()], md: `${MON[d.getUTCMonth()]} ${d.getUTCDate()}` };
	});
</script>

<div class="stats-app" data-theme={dark ? 'dark' : 'light'}>
	<div class="topbar">
		<div class="wrap">
			<div class="brand">Stats <span class="dot">· osmosis</span></div>
			<button class="tglbtn" aria-label="Toggle light / dark theme" onclick={() => (dark = !dark)}
				>◐</button
			>
		</div>
	</div>

	<div class="wrap">
		{#if loading}
			<p class="status">Loading…</p>
		{:else if loadError}
			<p class="status error">{loadError}</p>
		{:else}
			<!-- ===== TODAY ===== -->
			<section>
				<div class="sec-head">
					<h2>Today</h2>
					<span class="aside">{veganCt} vegan day{veganCt === 1 ? '' : 's'} this week</span>
				</div>
				<div class="card today">
					<div class="today-top">
						<div class="today-date">
							{#if typeof todayLabel === 'object'}
								<b>{todayLabel.wd}</b>, {todayLabel.md}
							{:else}{todayLabel}{/if}
						</div>
						{#if todayFd.logged && todayFd.category !== 'none'}
							<span class="chip">
								<span
									class="sw"
									style:background={catVar(todayFd.category, sugarLevel(todayFd.sugar))}
								></span>
								{CAT_LABEL[todayFd.category]}
							</span>
						{/if}
					</div>
					<div class="today-stats">
						{#each [{ k: 'Caffeine', v: todayFd.caffeine, u: 'mg', c: 'var(--caffeine)' }, { k: 'Alcohol', v: todayFd.alcohol, u: 'g', c: 'var(--alcohol)' }, { k: 'Sugar', v: todayFd.sugar, u: 'g', c: 'var(--sugar)' }] as s (s.k)}
							<div class="tstat">
								<div class="k"><span class="d" style:background={s.c}></span>{s.k}</div>
								<div class="v">{fmt(s.v)}<small>{s.u}</small></div>
							</div>
						{/each}
					</div>
				</div>
			</section>

			<!-- ===== SUBSTANCES ===== -->
			<section>
				<div class="sec-head">
					<h2>Substances</h2>
					<span class="aside">last 7 days · tap for times</span>
				</div>
				<div class="card">
					<div class="subs">
						{#each subRows as r (r.def.key)}
							<button
								class="srow"
								aria-expanded={expanded[r.def.key] ? 'true' : 'false'}
								onclick={() => toggleRow(r.def.key)}
							>
								<span class="name">
									<span class="d" style:background={r.def.color}></span>
									<span>{r.def.name}<span class="sub">{r.def.sub}</span></span>
								</span>
								<svg class="spark" viewBox="0 0 92 30" aria-hidden="true">
									<path d={r.spark.area} fill={r.def.color} opacity="0.1" />
									<path
										d={r.spark.line}
										fill="none"
										stroke={r.def.color}
										stroke-width="2"
										stroke-linecap="round"
										stroke-linejoin="round"
										opacity="0.85"
									/>
									<circle cx={r.spark.cx} cy={r.spark.cy} r="3" fill={r.def.color} />
								</svg>
								<span class="now">
									<span class="val">{fmt(r.now)}<small> {r.def.unit}</small></span>
									<span class="delta {r.deltaCls}">{r.deltaTxt} {r.def.unit}</span>
								</span>
							</button>
							<div class="srow-detail" class:open={expanded[r.def.key]}>
								{#if r.points.length > 0}
									<div class="pts">
										{#each r.points as p, i (i)}
											<div class="pt">
												<span class="t">{p.at ?? '—'}</span>
												<span class="bar"
													><i
														style:width={`${Math.round((p.value / r.pointsMax) * 100)}%`}
														style:background={r.def.color}
													></i></span
												>
												<span class="amt">{p.value} {r.def.unit}</span>
											</div>
										{/each}
									</div>
								{:else}
									<div class="pt none">None logged today.</div>
								{/if}
							</div>
						{/each}
					</div>
				</div>
			</section>

			<!-- ===== FOOD CALENDAR ===== -->
			<section>
				<div class="sec-head">
					<h2>Food</h2>
					<span class="aside">tap a day</span>
				</div>
				<FoodCalendar days={foodDays} {today} />
			</section>

			<div class="foot">notes.osmosis.page · glanceable stats</div>
		{/if}
	</div>
</div>

<style>
	/* ---- surfaces & ink (light, default) ---- */
	.stats-app {
		--plane: #f4f4f1;
		--surface: #ffffff;
		--surface-2: #faf9f7;
		--ink: #14140f;
		--ink-2: #57564f;
		--muted: #8b8a82;
		--hair: rgba(20, 20, 15, 0.09);
		--hair-strong: rgba(20, 20, 15, 0.16);

		--caffeine: #2a78d6;
		--alcohol: #6a5cd0;
		--sugar: #eb6834;

		/* food calendar: 3 hues x 4 sugar levels (light = darker means more sugar) */
		--vegan-0: #c9ecc9;
		--vegan-1: #79c879;
		--vegan-2: #2f9e2f;
		--vegan-3: #0c6b1e;
		--veg-0: #cfe2fb;
		--veg-1: #86b6ef;
		--veg-2: #2f7fdf;
		--veg-3: #184f95;
		--meat-0: #f7cdcb;
		--meat-1: #ef918f;
		--meat-2: #e34948;
		--meat-3: #a81f1f;

		--cell-empty: #ecece8;
		--cell-empty-line: var(--hair);

		/* cell date-number ink: pale cells (lvl 0-1) get dark ink, bold cells light */
		--num-lite: rgba(20, 20, 15, 0.55);
		--num-bold: rgba(255, 255, 255, 0.88);

		--good: #0a8f3c;
		--up: #c2691f;

		color: var(--ink);
		background: var(--plane);
		font:
			400 16px/1.45 system-ui,
			-apple-system,
			'Segoe UI',
			sans-serif;
		-webkit-font-smoothing: antialiased;
		min-height: 100vh;
		/* break out of the layout's content gutter for a full-bleed surface */
		margin: calc(-1 * var(--screen-gutter));
		padding: 0 0 96px;
	}

	/* dark: more sugar = brighter / more vivid (stays above the dark surface) */
	.stats-app[data-theme='dark'] {
		--plane: #0c0c0b;
		--surface: #171716;
		--surface-2: #1f1f1d;
		--ink: #f4f3ee;
		--ink-2: #bdbcb3;
		--muted: #86857d;
		--hair: rgba(255, 255, 255, 0.1);
		--hair-strong: rgba(255, 255, 255, 0.18);
		--caffeine: #4f97ec;
		--alcohol: #9d92ec;
		--sugar: #f0805a;
		--vegan-0: #20351f;
		--vegan-1: #2f7a34;
		--vegan-2: #33ad3f;
		--vegan-3: #5fe06a;
		--veg-0: #1c3149;
		--veg-1: #285a90;
		--veg-2: #3987e5;
		--veg-3: #74b1f2;
		--meat-0: #4a1f1e;
		--meat-1: #8f302f;
		--meat-2: #e05150;
		--meat-3: #f28d8c;
		--cell-empty: #24241f;
		--cell-empty-line: var(--hair);
		/* inverted: low levels are dark cells (light ink), high levels bright (dark ink) */
		--num-lite: rgba(244, 243, 238, 0.82);
		--num-bold: rgba(12, 12, 11, 0.78);
		--good: #35c46a;
		--up: #e0a15a;
	}

	.wrap {
		max-width: 460px;
		margin: 0 auto;
		padding: 0 16px;
	}

	/* ---- top bar ---- */
	.topbar {
		position: sticky;
		top: 0;
		z-index: 5;
		background: color-mix(in srgb, var(--plane) 86%, transparent);
		backdrop-filter: blur(10px);
		border-bottom: 1px solid var(--hair);
	}
	.topbar .wrap {
		display: flex;
		align-items: center;
		justify-content: space-between;
		height: 56px;
	}
	.brand {
		font-weight: 600;
		letter-spacing: -0.01em;
		font-size: 17px;
	}
	.brand .dot {
		color: var(--muted);
		font-weight: 400;
	}
	.tglbtn {
		appearance: none;
		border: 1px solid var(--hair-strong);
		background: var(--surface);
		color: var(--ink-2);
		height: 36px;
		min-width: 36px;
		border-radius: 10px;
		font-size: 15px;
		cursor: pointer;
		display: grid;
		place-items: center;
		padding: 0 8px;
	}
	.tglbtn:active {
		transform: scale(0.96);
	}

	section {
		margin-top: 22px;
	}
	.sec-head {
		display: flex;
		align-items: baseline;
		justify-content: space-between;
		margin: 0 0 10px;
	}
	.sec-head h2 {
		font-size: 13px;
		font-weight: 600;
		letter-spacing: 0.04em;
		text-transform: uppercase;
		color: var(--muted);
		margin: 0;
	}
	.sec-head .aside {
		font-size: 12px;
		color: var(--muted);
	}
	.status {
		margin-top: 40px;
		text-align: center;
		color: var(--muted);
	}
	.status.error {
		color: #d0453f;
	}

	.card {
		background: var(--surface);
		border: 1px solid var(--hair);
		border-radius: 16px;
		padding: 16px;
	}

	/* ---- TODAY hero ---- */
	.today {
		display: flex;
		flex-direction: column;
		gap: 14px;
	}
	.today-top {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 12px;
	}
	.today-date {
		font-size: 13px;
		color: var(--ink-2);
	}
	.today-date b {
		color: var(--ink);
		font-weight: 600;
	}
	.chip {
		display: inline-flex;
		align-items: center;
		gap: 8px;
		padding: 7px 12px 7px 10px;
		border-radius: 999px;
		font-size: 13.5px;
		font-weight: 600;
		border: 1px solid var(--hair);
		background: var(--surface-2);
	}
	.chip .sw {
		width: 12px;
		height: 12px;
		border-radius: 4px;
	}
	.today-stats {
		display: grid;
		grid-template-columns: repeat(3, 1fr);
		gap: 10px;
	}
	.tstat {
		background: var(--surface-2);
		border: 1px solid var(--hair);
		border-radius: 12px;
		padding: 10px 10px 11px;
	}
	.tstat .k {
		font-size: 11.5px;
		color: var(--muted);
		display: flex;
		align-items: center;
		gap: 6px;
	}
	.tstat .k .d {
		width: 8px;
		height: 8px;
		border-radius: 50%;
	}
	.tstat .v {
		margin-top: 5px;
		font-size: 22px;
		font-weight: 600;
		letter-spacing: -0.02em;
	}
	.tstat .v small {
		font-size: 12px;
		font-weight: 500;
		color: var(--muted);
		margin-left: 2px;
	}

	/* ---- substance rows ---- */
	.subs {
		display: flex;
		flex-direction: column;
		gap: 0;
		padding: 4px 0;
	}
	.srow {
		display: grid;
		grid-template-columns: 1fr auto auto;
		align-items: center;
		gap: 12px;
		width: 100%;
		text-align: left;
		background: none;
		border: 0;
		cursor: pointer;
		padding: 13px 8px;
		border-radius: 12px;
		color: inherit;
		font: inherit;
		min-height: 56px;
	}
	.srow + .srow-detail + .srow {
		border-top: 1px solid var(--hair);
	}
	.srow:active {
		background: var(--surface-2);
	}
	.srow[aria-expanded='true'] {
		background: var(--surface-2);
	}
	.srow .name {
		display: flex;
		align-items: center;
		gap: 10px;
		font-size: 15px;
		font-weight: 500;
	}
	.srow .name .d {
		width: 10px;
		height: 10px;
		border-radius: 50%;
		flex: none;
	}
	.srow .name .sub {
		display: block;
		font-size: 11.5px;
		color: var(--muted);
		font-weight: 400;
	}
	.srow .spark {
		width: 92px;
		height: 30px;
	}
	.srow .now {
		text-align: right;
		min-width: 58px;
	}
	.srow .now .val {
		font-size: 16px;
		font-weight: 600;
		letter-spacing: -0.01em;
	}
	.srow .now .val small {
		font-size: 11px;
		color: var(--muted);
		font-weight: 500;
	}
	.srow .now .delta {
		display: block;
		font-size: 11px;
		color: var(--muted);
	}
	.srow .now .delta.up {
		color: var(--up);
	}
	.srow .now .delta.dn {
		color: var(--good);
	}

	.srow-detail {
		display: none;
		padding: 2px 8px 14px 34px;
	}
	.srow-detail.open {
		display: block;
	}
	.pts {
		display: flex;
		flex-direction: column;
		gap: 0;
	}
	.pt {
		display: flex;
		align-items: center;
		gap: 10px;
		padding: 7px 0;
		border-top: 1px dashed var(--hair);
		font-size: 13px;
	}
	.pt:first-child {
		border-top: 0;
	}
	.pt.none {
		border: 0;
		color: var(--muted);
	}
	.pt .t {
		color: var(--muted);
		width: 52px;
		font-variant-numeric: tabular-nums;
	}
	.pt .bar {
		flex: 1;
		height: 6px;
		border-radius: 3px;
		background: var(--surface);
		border: 1px solid var(--hair);
		overflow: hidden;
	}
	.pt .bar i {
		display: block;
		height: 100%;
		border-radius: 3px;
	}
	.pt .amt {
		width: 56px;
		text-align: right;
		font-variant-numeric: tabular-nums;
		color: var(--ink-2);
	}

	.foot {
		text-align: center;
		color: var(--muted);
		font-size: 11px;
		margin-top: 28px;
	}
</style>
