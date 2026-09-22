<script lang="ts">
	// Mobile-first food calendar: 7 weekday COLUMNS (Mon-first), weeks flowing
	// DOWNWARD, grouped into labeled month blocks — no horizontal scroll, cells
	// stay a comfortable tap size. Hue = diet category, depth = that day's sugar
	// (4 bins). Tapping a day opens a bottom sheet with the day's detail.
	// Inline layout, no chart deps. Colours come from CSS custom properties on
	// the surrounding `.stats-app` (which flips them per light/dark theme), so
	// the dark ramp is a *selected* inversion, not an automatic flip.
	import { buildCalendarMonths, sugarLevel, SUGAR_BIN_LABELS, type FoodDay } from './stats';

	interface Props {
		/** Contiguous days across the calendar range (logged and unlogged). */
		days: FoodDay[];
		/** today (YYYY-MM-DD), gets a ring. */
		today: string;
	}

	let { days, today }: Props = $props();

	const CAT_LABEL = {
		vegan: 'Vegan',
		vegetarian: 'Vegetarian',
		meat: 'Meat',
		none: 'No food'
	} as const;
	// Category → CSS ramp stem (the `vegetarian` hue vars are named `veg`).
	const CAT_STEM = { vegan: 'vegan', vegetarian: 'veg', meat: 'meat' } as const;

	function catVar(cat: 'vegan' | 'vegetarian' | 'meat', lvl: number): string {
		return `var(--${CAT_STEM[cat]}-${lvl})`;
	}

	const dayMap = $derived(new Map(days.map((d) => [d.date, d])));
	const months = $derived(
		days.length === 0 ? [] : buildCalendarMonths(days[0].date, days[days.length - 1].date, today)
	);

	function dayNum(date: string): number {
		return Number(date.slice(8, 10));
	}

	// --- bottom sheet ---
	let selected = $state<FoodDay | null>(null);

	function open(fd: FoodDay) {
		selected = fd;
	}
	function close() {
		selected = null;
	}
	function onKeydown(e: KeyboardEvent) {
		if (e.key === 'Escape') close();
	}

	const WEEKDAYS = ['M', 'T', 'W', 'T', 'F', 'S', 'S'];

	/** [0, 1, …, n-1] — leading-blank placeholders, keyed by index. */
	function range(n: number): number[] {
		return [...Array(n).keys()];
	}
</script>

<svelte:window onkeydown={onKeydown} />

<div class="card cal-wrap">
	<div class="weekdays">
		{#each WEEKDAYS as wd, i (i)}<span>{wd}</span>{/each}
	</div>
	<div class="cal">
		{#each months as mo (mo.key)}
			<div class="month">
				<div class="month-lab">{mo.label}</div>
				<div class="grid">
					{#each range(mo.leadingBlanks) as b (b)}
						<div class="cell blank"></div>
					{/each}
					{#each mo.dates as date (date)}
						{@const fd = dayMap.get(date)}
						{@const cat = fd?.category ?? 'none'}
						{@const lvl = fd ? sugarLevel(fd.sugar) : 0}
						{#if fd && fd.logged && cat !== 'none'}
							<button
								class="cell"
								class:lite={lvl <= 1}
								class:today={date === today}
								style:background={catVar(cat, lvl)}
								aria-label={`${date}, ${CAT_LABEL[cat]}, sugar ${fd.sugar} g`}
								onclick={() => open(fd)}
							>
								<span class="num">{dayNum(date)}</span>
							</button>
						{:else}
							<button
								class="cell empty"
								class:today={date === today}
								aria-label={`${date}, nothing logged`}
								onclick={() => fd && open(fd)}
							>
								<span class="num">{dayNum(date)}</span>
							</button>
						{/if}
					{/each}
				</div>
			</div>
		{/each}
	</div>
</div>

<div class="legend">
	<h3>Category · sugar</h3>
	{#each [['vegan', 'Vegan'], ['veg', 'Vegetarian'], ['meat', 'Meat']] as [stem, name] (stem)}
		<div class="lg-row">
			<span class="lg-name">{name}</span>
			<span class="lg-ramp">
				{#each [0, 1, 2, 3] as l (l)}<i style:background={`var(--${stem}-${l})`}></i>{/each}
			</span>
		</div>
	{/each}
	<div class="lg-scale">
		<span>less sugar</span>
		{#each SUGAR_BIN_LABELS as b (b)}<span>{b}</span>{/each}
	</div>
	<div class="lg-note">
		Hue = what you ate · depth = added sugar that day. A dashed cell means nothing was logged.
	</div>
</div>

<!-- day sheet -->
<div
	class="scrim"
	class:open={selected !== null}
	onclick={close}
	role="presentation"
	aria-hidden="true"
></div>
<div
	class="sheet"
	class:open={selected !== null}
	role="dialog"
	aria-modal="true"
	aria-label="Day detail"
>
	<div class="grabber"></div>
	{#if selected}
		{@const lvl = sugarLevel(selected.sugar)}
		<div class="sheet-h">
			<div>
				<div class="dt">{selected.date}</div>
			</div>
			{#if selected.logged && selected.category !== 'none'}
				<span class="chip">
					<span class="sw" style:background={catVar(selected.category, lvl)}></span>
					{CAT_LABEL[selected.category]}
				</span>
			{/if}
		</div>
		{#if selected.logged && selected.category !== 'none'}
			<div class="sheet-metrics">
				<div class="smrow">
					<span class="ic" style:background="var(--sugar)"></span>
					<span class="lab">Sugar<span class="s">level {lvl + 1} of 4</span></span>
					<span class="num">{selected.sugar}<small> g</small></span>
				</div>
				<div class="smrow">
					<span class="ic" style:background="var(--caffeine)"></span>
					<span class="lab"
						>Caffeine<span class="s"
							>{selected.caffeineCount} intake{selected.caffeineCount === 1 ? '' : 's'}</span
						></span
					>
					<span class="num">{selected.caffeine}<small> mg</small></span>
				</div>
				<div class="smrow">
					<span class="ic" style:background="var(--alcohol)"></span>
					<span class="lab"
						>Alcohol<span class="s"
							>{selected.alcohol
								? `${selected.alcoholCount} drink${selected.alcoholCount === 1 ? '' : 's'}`
								: 'none'}</span
						></span
					>
					<span class="num">{selected.alcohol}<small> g</small></span>
				</div>
			</div>
		{:else}
			<div class="sheet-empty">
				<div class="big">○</div>
				No food logged this day.<br />Sugar and substance totals aren’t available.
			</div>
		{/if}
	{/if}
</div>

<style>
	.card {
		background: var(--surface);
		border: 1px solid var(--hair);
		border-radius: 16px;
		padding: 16px;
	}
	.weekdays {
		display: grid;
		grid-template-columns: repeat(7, 1fr);
		gap: 5px;
		padding: 0 2px 8px;
	}
	.weekdays span {
		text-align: center;
		font-size: 11px;
		color: var(--muted);
		font-weight: 500;
	}
	.month-lab {
		font-size: 12px;
		font-weight: 600;
		color: var(--ink-2);
		margin: 12px 2px 7px;
		letter-spacing: 0.01em;
	}
	.month:first-child .month-lab {
		margin-top: 2px;
	}
	.grid {
		display: grid;
		grid-template-columns: repeat(7, 1fr);
		gap: 5px;
	}
	.cell {
		aspect-ratio: 1 / 1;
		border-radius: 9px;
		border: 1px solid transparent;
		background: var(--cell-empty);
		position: relative;
		cursor: pointer;
		padding: 0;
		transition: transform 0.06s ease;
	}
	.cell.empty {
		background: transparent;
		border: 1px dashed var(--cell-empty-line);
		cursor: default;
	}
	.cell.blank {
		background: transparent;
		border: 0;
		cursor: default;
	}
	.cell.today {
		box-shadow:
			0 0 0 2px var(--surface),
			0 0 0 4px var(--ink);
	}
	.cell:not(.empty):not(.blank):active {
		transform: scale(0.9);
	}
	.cell .num {
		position: absolute;
		top: 3px;
		left: 5px;
		font-size: 9.5px;
		line-height: 1;
		font-variant-numeric: tabular-nums;
		color: var(--num-bold);
	}
	.cell.lite .num {
		color: var(--num-lite);
	}
	.cell.empty .num {
		color: var(--muted);
	}

	.legend {
		margin-top: 16px;
		background: var(--surface-2);
		border: 1px solid var(--hair);
		border-radius: 14px;
		padding: 14px;
	}
	.legend h3 {
		margin: 0 0 10px;
		font-size: 12px;
		font-weight: 600;
		letter-spacing: 0.03em;
		text-transform: uppercase;
		color: var(--muted);
	}
	.lg-row {
		display: flex;
		align-items: center;
		gap: 10px;
		margin-bottom: 9px;
	}
	.lg-name {
		width: 82px;
		font-size: 13px;
		font-weight: 500;
	}
	.lg-ramp {
		display: flex;
		gap: 3px;
		flex: 1;
	}
	.lg-ramp i {
		height: 16px;
		flex: 1;
		border-radius: 4px;
	}
	.lg-scale {
		display: flex;
		justify-content: space-between;
		font-size: 10.5px;
		color: var(--muted);
		margin: 3px 0 0 92px;
	}
	.lg-note {
		font-size: 11px;
		color: var(--muted);
		margin-top: 11px;
		line-height: 1.4;
		border-top: 1px solid var(--hair);
		padding-top: 10px;
	}

	/* day sheet */
	.scrim {
		position: fixed;
		inset: 0;
		background: rgba(0, 0, 0, 0.42);
		opacity: 0;
		pointer-events: none;
		transition: opacity 0.2s ease;
		z-index: 20;
	}
	.scrim.open {
		opacity: 1;
		pointer-events: auto;
	}
	.sheet {
		position: fixed;
		left: 0;
		right: 0;
		bottom: 0;
		z-index: 21;
		background: var(--surface);
		border-top-left-radius: 22px;
		border-top-right-radius: 22px;
		border-top: 1px solid var(--hair-strong);
		transform: translateY(100%);
		transition: transform 0.24s cubic-bezier(0.32, 0.72, 0, 1);
		max-width: 460px;
		margin: 0 auto;
		padding: 8px 18px calc(24px + env(safe-area-inset-bottom));
		box-shadow: 0 -12px 40px rgba(0, 0, 0, 0.22);
		max-height: 82vh;
		overflow: auto;
	}
	.sheet.open {
		transform: translateY(0);
	}
	.grabber {
		width: 38px;
		height: 4px;
		border-radius: 2px;
		background: var(--hair-strong);
		margin: 0 auto 14px;
	}
	.sheet-h {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 12px;
		margin-bottom: 4px;
	}
	.sheet-h .dt {
		font-size: 19px;
		font-weight: 600;
		letter-spacing: -0.01em;
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
	.sheet-metrics {
		display: flex;
		flex-direction: column;
		gap: 0;
		margin-top: 12px;
	}
	.smrow {
		display: flex;
		align-items: center;
		gap: 12px;
		padding: 12px 0;
		border-top: 1px solid var(--hair);
	}
	.smrow .ic {
		width: 9px;
		height: 9px;
		border-radius: 50%;
		flex: none;
	}
	.smrow .lab {
		flex: 1;
		font-size: 14px;
	}
	.smrow .lab .s {
		display: block;
		font-size: 11.5px;
		color: var(--muted);
	}
	.smrow .num {
		font-size: 16px;
		font-weight: 600;
		font-variant-numeric: tabular-nums;
	}
	.smrow .num small {
		font-size: 11px;
		color: var(--muted);
		font-weight: 500;
	}
	.sheet-empty {
		text-align: center;
		color: var(--muted);
		font-size: 13.5px;
		padding: 22px 8px 10px;
	}
	.sheet-empty .big {
		font-size: 26px;
		margin-bottom: 6px;
	}
</style>
