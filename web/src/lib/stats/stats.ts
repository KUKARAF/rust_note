// Types + small pure helpers for the /stats board. Mirrors the shape the
// server's `GET /api/stats` and `/api/stats/registry` return (see
// crates/server/src/stats/ and crates/core/src/stats.rs).

export type ChartKind = 'line' | 'bar' | 'boolean' | 'heatmap';
export type AggKind = 'sum' | 'last' | 'max' | 'min' | 'count';

export const CHART_KINDS: ChartKind[] = ['line', 'bar', 'boolean', 'heatmap'];
export const AGG_KINDS: AggKind[] = ['sum', 'last', 'max', 'min', 'count'];

export interface StatPoint {
	value: number;
	/** "HH:MM" — omitted for untimed samples. */
	at?: string;
}

export interface DayValue {
	date: string; // YYYY-MM-DD
	value: number; // aggregated per the metric's `agg`
	points?: StatPoint[];
}

export interface Series {
	metric: string;
	label: string;
	unit: string;
	chart: string; // ChartKind, but server strings are trusted loosely
	agg: string;
	days: DayValue[];
}

export interface StatsResponse {
	series: Series[];
}

/** A per-user metric display definition (the registry). */
export interface MetricDef {
	metric: string;
	unit: string;
	label: string;
	chart: string;
	agg: string;
}

/**
 * `?from=…&to=…` for `GET /api/stats`, or an empty string when neither bound
 * is set (the server then falls back to its own default window). Lives here
 * rather than in the component because a `URLSearchParams` instance held
 * inside a Svelte component would have to be the reactive
 * `SvelteURLSearchParams` — this one is a throwaway used to build a string.
 */
export function statsRangeQuery(from: string, to: string): string {
	const params = new URLSearchParams();
	if (from.trim()) params.set('from', from.trim());
	if (to.trim()) params.set('to', to.trim());
	const query = params.toString();
	return query ? `?${query}` : '';
}

// ---- Food calendar (substances + food view) --------------------------------

/**
 * Defensive registry defaults for the substances/food view. Registered on the
 * stats page load if absent (idempotent PUT), never clobbering existing defs.
 * `vegan`/`vegetarian` are boolean-per-day (parent query); `sugar` is a timed
 * metric summed per day.
 */
export const FOOD_REGISTRY_DEFAULTS: MetricDef[] = [
	{ metric: 'vegan', unit: '', label: 'Vegan', chart: 'boolean', agg: 'last' },
	{ metric: 'vegetarian', unit: '', label: 'Vegetarian', chart: 'boolean', agg: 'last' },
	{ metric: 'sugar', unit: 'g', label: 'Sugar', chart: 'bar', agg: 'sum' }
];

/** Metrics shown in the "Substances" group, in display order. */
export const SUBSTANCE_METRICS = ['caffeine', 'alcohol', 'sugar'] as const;

/** A day's food category, derived from its `vegan`/`vegetarian` booleans. */
export type FoodCategory = 'vegan' | 'vegetarian' | 'meat' | 'none';

/**
 * Category for a day from its `vegan`/`vegetarian` booleans (each 1 = true,
 * 0 = present-but-false, `undefined` = the key wasn't recorded that day):
 * vegan wins over vegetarian; if food data exists but neither flag is true the
 * day is `meat`; a day with no food data at all is `none`.
 */
export function foodCategory(
	vegan: number | undefined,
	vegetarian: number | undefined
): FoodCategory {
	if (vegan === undefined && vegetarian === undefined) return 'none';
	if (vegan === 1) return 'vegan';
	if (vegetarian === 1) return 'vegetarian';
	return 'meat';
}

/**
 * Sugar grams → discrete shade index 0..3 within a category hue. Four bins,
 * matching the legend: 0–20 / 20–40 / 40–65 / 65 g+. In light mode a higher
 * level is a darker shade; in dark mode the ramp is inverted (higher = more
 * vivid) so heavy-sugar days don't sink into the surface — that inversion lives
 * in the CSS custom-property ramps, not here.
 */
export function sugarLevel(grams: number): 0 | 1 | 2 | 3 {
	if (grams < 20) return 0;
	if (grams < 40) return 1;
	if (grams < 65) return 2;
	return 3;
}

/** Human labels for the four sugar bins (index-aligned with {@link sugarLevel}). */
export const SUGAR_BIN_LABELS = ['0–20', '20–40', '40–65', '65 g+'] as const;

/** Build a `date → aggregated value` map from a series' days. */
export function dayValueMap(series: Series | undefined): Map<string, number> {
	const map = new Map<string, number>();
	if (series) for (const d of series.days) map.set(d.date, d.value);
	return map;
}

/** Build a `date → timed points` map from a series' days. */
export function dayPointsMap(series: Series | undefined): Map<string, StatPoint[]> {
	const map = new Map<string, StatPoint[]>();
	if (series) for (const d of series.days) map.set(d.date, d.points ?? []);
	return map;
}

/** A day resolved for the food calendar and its detail sheet. */
export interface FoodDay {
	date: string; // YYYY-MM-DD
	category: FoodCategory;
	/** true when food was logged that day (category !== 'none'). */
	logged: boolean;
	sugar: number;
	caffeine: number;
	alcohol: number;
	/** number of timed samples that day, for the sheet's sub-labels. */
	caffeineCount: number;
	alcoholCount: number;
}

// -- date helpers (UTC-based to avoid TZ drift on YYYY-MM-DD strings) --

/** `YYYY-MM-DD` → a UTC `Date` at midnight (null when malformed). */
export function parseISODate(s: string): Date | null {
	const m = /^(\d{4})-(\d{2})-(\d{2})$/.exec(s);
	if (!m) return null;
	const d = new Date(Date.UTC(Number(m[1]), Number(m[2]) - 1, Number(m[3])));
	return Number.isNaN(d.getTime()) ? null : d;
}

/** A UTC `Date` → `YYYY-MM-DD`. */
export function toISODate(d: Date): string {
	return `${d.getUTCFullYear()}-${String(d.getUTCMonth() + 1).padStart(2, '0')}-${String(d.getUTCDate()).padStart(2, '0')}`;
}

/** Weekday index with Monday = 0 … Sunday = 6. */
export function mondayIndex(d: Date): number {
	return (d.getUTCDay() + 6) % 7;
}

export function addDays(d: Date, n: number): Date {
	return new Date(d.getTime() + n * 86_400_000);
}

const MONTH_ABBR = [
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

/** The `n` calendar dates ending at (and including) `end`, oldest first. */
export function lastNDates(end: string, n: number): string[] {
	const endD = parseISODate(end) ?? new Date();
	const out: string[] = [];
	for (let i = n - 1; i >= 0; i--) out.push(toISODate(addDays(endD, -i)));
	return out;
}

/**
 * A labeled month block for the mobile calendar (weekday columns, weeks flowing
 * downward). `leadingBlanks` pads the first row so the month's first in-range
 * day sits under its weekday column (Monday-first).
 */
export interface CalendarMonth {
	key: string; // "2026-8" (0-based month)
	label: string; // "Sep 2026"
	leadingBlanks: number;
	dates: string[]; // in-range YYYY-MM-DD dates of this month, in order
}

/**
 * Group the inclusive range `start`..`end` (`YYYY-MM-DD`) into month blocks for
 * the transposed mobile calendar. Falls back to a ~6-week window ending today
 * when the range is missing/invalid.
 */
export function buildCalendarMonths(start: string, end: string, today: string): CalendarMonth[] {
	let endD = parseISODate(end) ?? parseISODate(today) ?? new Date();
	let startD = parseISODate(start) ?? addDays(endD, -41);
	if (startD > endD) [startD, endD] = [endD, startD];

	const months: CalendarMonth[] = [];
	let cursor = startD;
	let current: CalendarMonth | null = null;
	while (cursor <= endD) {
		const y = cursor.getUTCFullYear();
		const m = cursor.getUTCMonth();
		const key = `${y}-${m}`;
		if (current === null || current.key !== key) {
			current = {
				key,
				label: `${MONTH_ABBR[m]} ${y}`,
				leadingBlanks: mondayIndex(cursor),
				dates: []
			};
			months.push(current);
		}
		current.dates.push(toISODate(cursor));
		cursor = addDays(cursor, 1);
	}
	return months;
}

/** Geometry for a compact area sparkline (values oldest→newest). */
export interface Sparkline {
	line: string;
	area: string;
	cx: number;
	cy: number;
}
export function sparkline(vals: number[], w: number, h: number, pad = 3): Sparkline {
	if (vals.length === 0) return { line: '', area: '', cx: w / 2, cy: h / 2 };
	const max = Math.max(1, ...vals);
	const n = vals.length;
	const xAt = (i: number) => (n <= 1 ? w / 2 : pad + (i * (w - 2 * pad)) / (n - 1));
	const yAt = (v: number) => h - pad - (v / max) * (h - 2 * pad);
	const line = vals
		.map((v, i) => `${i ? 'L' : 'M'}${xAt(i).toFixed(1)} ${yAt(v).toFixed(1)}`)
		.join(' ');
	const area = `${line} L${xAt(n - 1).toFixed(1)} ${(h - pad).toFixed(1)} L${xAt(0).toFixed(1)} ${(h - pad).toFixed(1)} Z`;
	return { line, area, cx: xAt(n - 1), cy: yAt(vals[n - 1]) };
}

/** min/max across a series' day values, padded so a flat series still draws. */
export function valueRange(days: DayValue[]): { min: number; max: number } {
	if (days.length === 0) return { min: 0, max: 1 };
	let min = Infinity;
	let max = -Infinity;
	for (const d of days) {
		if (d.value < min) min = d.value;
		if (d.value > max) max = d.value;
	}
	min = Math.min(min, 0);
	if (max === min) max = min + 1;
	return { min, max };
}
