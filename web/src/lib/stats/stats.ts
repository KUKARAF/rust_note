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

/** Grams of sugar mapped to the darkest shade of a hue. */
export const SUGAR_CAP_G = 80;

/**
 * Sugar grams → discrete shade index 0..4 within a category hue (0 = lightest
 * at 0g, 4 = darkest at/above {@link SUGAR_CAP_G}). GitHub-contribution-style
 * quintiles across 0–80g.
 */
export function sugarLevel(grams: number): 0 | 1 | 2 | 3 | 4 {
	if (grams <= 0) return 0;
	if (grams <= 20) return 1;
	if (grams <= 40) return 2;
	if (grams <= 60) return 3;
	return 4;
}

/** Build a `date → aggregated value` map from a series' days. */
export function dayValueMap(series: Series | undefined): Map<string, number> {
	const map = new Map<string, number>();
	if (series) for (const d of series.days) map.set(d.date, d.value);
	return map;
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

function addDays(d: Date, n: number): Date {
	return new Date(d.getTime() + n * 86_400_000);
}

/** One cell of the food calendar (a real day, or a padding slot). */
export interface CalendarCell {
	/** `YYYY-MM-DD`, or null for a leading/trailing padding slot. */
	date: string | null;
}

/**
 * Lay dates from `start`..`end` (inclusive `YYYY-MM-DD`) into GitHub-style
 * columns of weeks × 7 weekday rows (Monday top). Leading/trailing slots that
 * fall outside the range are padding cells with a null date. Falls back to a
 * ~12-week window ending today when the range is missing/invalid.
 */
export function buildCalendarWeeks(start: string, end: string, today: string): CalendarCell[][] {
	let endD = parseISODate(end) ?? parseISODate(today) ?? new Date();
	let startD = parseISODate(start) ?? addDays(endD, -83);
	if (startD > endD) [startD, endD] = [endD, startD];
	// Snap the first column to the Monday on/before the start date.
	const gridStart = addDays(startD, -mondayIndex(startD));
	const weeks: CalendarCell[][] = [];
	let cursor = gridStart;
	while (cursor <= endD) {
		const week: CalendarCell[] = [];
		for (let row = 0; row < 7; row++) {
			const inRange = cursor >= startD && cursor <= endD;
			week.push({ date: inRange ? toISODate(cursor) : null });
			cursor = addDays(cursor, 1);
		}
		weeks.push(week);
	}
	return weeks;
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
