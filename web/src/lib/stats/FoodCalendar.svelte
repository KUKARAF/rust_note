<script lang="ts">
	// GitHub-contribution-style food calendar: weeks as columns, weekdays as
	// rows. Each day's HUE encodes its category (vegan/vegetarian/meat), and the
	// SHADE within that hue darkens as the day's total sugar (grams) rises.
	// Inline SVG, no chart deps. Styling leans on the site's design tokens so it
	// rides along with the active theme.
	import {
		buildCalendarWeeks,
		foodCategory,
		sugarLevel,
		SUGAR_CAP_G,
		type FoodCategory
	} from './stats';

	interface Props {
		/** date (YYYY-MM-DD) → 1 (true) / 0 (present-false) for the `vegan` flag. */
		veganMap: Map<string, number>;
		/** date → 1/0 for the `vegetarian` flag. */
		vegetarianMap: Map<string, number>;
		/** date → grams of sugar that day. */
		sugarMap: Map<string, number>;
		/** inclusive range (YYYY-MM-DD); empty falls back to a ~12-week window. */
		start: string;
		end: string;
		/** today (YYYY-MM-DD), used for the fallback window. */
		today: string;
	}

	let { veganMap, vegetarianMap, sugarMap, start, end, today }: Props = $props();

	// Five shades per hue, lightest (0g sugar) → darkest (>= cap). Chosen to stay
	// legible on the dark terminal surface while reading as one hue ramp; the
	// legend + per-cell tooltip carry the category as text so identity is never
	// conveyed by colour alone.
	const RAMPS: Record<Exclude<FoodCategory, 'none'>, [string, string, string, string, string]> = {
		vegan: ['#57e389', '#39d353', '#26a641', '#1a7f37', '#0e4429'],
		vegetarian: ['#a5d6ff', '#58a6ff', '#388bfd', '#1f6feb', '#1158c7'],
		meat: ['#ffb3b0', '#ff7b72', '#f85149', '#da3633', '#b62324']
	};
	const EMPTY_FILL = 'var(--kv-faint)';

	const CATEGORY_LABEL: Record<FoodCategory, string> = {
		vegan: 'Vegan',
		vegetarian: 'Vegetarian',
		meat: 'Meat / non-veg',
		none: 'No food data'
	};

	// Geometry.
	const CELL = 12;
	const GAP = 3;
	const STEP = CELL + GAP;
	const TOP = 16; // month-label band
	const LEFT = 26; // weekday-label gutter

	const weeks = $derived(buildCalendarWeeks(start, end, today));
	const width = $derived(LEFT + weeks.length * STEP);
	const height = TOP + 7 * STEP;

	const WEEKDAYS = ['Mon', '', 'Wed', '', 'Fri', '', ''];
	const MONTHS = [
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

	interface Rendered {
		date: string | null;
		x: number;
		y: number;
		fill: string;
		title: string;
	}

	function cellFor(date: string | null): { fill: string; title: string } {
		if (date === null) return { fill: 'none', title: '' };
		const cat = foodCategory(veganMap.get(date), vegetarianMap.get(date));
		const sugar = sugarMap.get(date) ?? 0;
		if (cat === 'none') {
			const known = sugarMap.has(date);
			return {
				fill: EMPTY_FILL,
				title: `${date} · ${CATEGORY_LABEL.none}${known ? ` · sugar ${sugar} g` : ''}`
			};
		}
		const fill = RAMPS[cat][sugarLevel(sugar)];
		return { fill, title: `${date} · ${CATEGORY_LABEL[cat]} · sugar ${sugar} g` };
	}

	const cells = $derived.by<Rendered[]>(() => {
		const out: Rendered[] = [];
		weeks.forEach((week, col) => {
			week.forEach((cell, row) => {
				const { fill, title } = cellFor(cell.date);
				out.push({
					date: cell.date,
					x: LEFT + col * STEP,
					y: TOP + row * STEP,
					fill,
					title
				});
			});
		});
		return out;
	});

	// Month labels: one per column whose Monday (row 0) starts a new month.
	interface MonthLabel {
		x: number;
		text: string;
	}
	const monthLabels = $derived.by<MonthLabel[]>(() => {
		const out: MonthLabel[] = [];
		let lastMonth = -1;
		weeks.forEach((week, col) => {
			const first = week.find((c) => c.date !== null)?.date;
			if (!first) return;
			const month = Number(first.slice(5, 7)) - 1;
			if (month !== lastMonth) {
				out.push({ x: LEFT + col * STEP, text: MONTHS[month] });
				lastMonth = month;
			}
		});
		return out;
	});

	// Legend sugar-scale swatches use the vegan hue as the reference ramp.
	const scaleSwatches = RAMPS.vegan;
</script>

<div class="food-cal">
	<div class="scroll">
		<svg
			{width}
			{height}
			viewBox={`0 0 ${width} ${height}`}
			role="img"
			aria-label="Food calendar: day colour is diet category, darkness is sugar intake"
		>
			{#each monthLabels as m (m.x)}
				<text x={m.x} y={11} class="axis">{m.text}</text>
			{/each}
			{#each WEEKDAYS as day, row (row)}
				{#if day}
					<text x={0} y={TOP + row * STEP + CELL - 2} class="axis">{day}</text>
				{/if}
			{/each}
			{#each cells as c (`${c.x},${c.y}`)}
				{#if c.date !== null}
					<rect x={c.x} y={c.y} width={CELL} height={CELL} rx="2" fill={c.fill} class="cell">
						<title>{c.title}</title>
					</rect>
				{/if}
			{/each}
		</svg>
	</div>

	<div class="legend">
		<div class="legend-row">
			<span class="legend-title">Category</span>
			<span class="key"><span class="sw" style:background={RAMPS.vegan[2]}></span>Vegan</span>
			<span class="key"
				><span class="sw" style:background={RAMPS.vegetarian[2]}></span>Vegetarian</span
			>
			<span class="key"><span class="sw" style:background={RAMPS.meat[2]}></span>Meat</span>
			<span class="key"><span class="sw" style:background={EMPTY_FILL}></span>No data</span>
		</div>
		<div class="legend-row">
			<span class="legend-title">Sugar</span>
			<span class="scale-cap">0 g</span>
			{#each scaleSwatches as s, i (i)}
				<span class="sw" style:background={s} title={`level ${i}`}></span>
			{/each}
			<span class="scale-cap">{SUGAR_CAP_G}+ g</span>
		</div>
	</div>
</div>

<style>
	.food-cal {
		display: flex;
		flex-direction: column;
		gap: var(--space-4);
	}
	.scroll {
		overflow-x: auto;
		padding-bottom: var(--space-1);
	}
	svg {
		display: block;
	}
	.cell {
		stroke: rgba(0, 0, 0, 0.25);
		stroke-width: 1;
	}
	.axis {
		fill: var(--kv-dim);
		font-family: var(--font-term);
		font-size: 11px;
	}
	.legend {
		display: flex;
		flex-direction: column;
		gap: var(--space-2);
		font-family: var(--font-term);
		font-size: var(--type-meta);
		color: var(--kv-dim);
	}
	.legend-row {
		display: flex;
		align-items: center;
		flex-wrap: wrap;
		gap: var(--space-3);
	}
	.legend-title {
		color: var(--kv-ink);
		min-width: 4.5rem;
	}
	.key {
		display: inline-flex;
		align-items: center;
		gap: var(--space-1);
	}
	.sw {
		width: 12px;
		height: 12px;
		border-radius: var(--radius-control);
		display: inline-block;
		border: 1px solid rgba(0, 0, 0, 0.25);
	}
	.scale-cap {
		color: var(--kv-dim);
	}
</style>
