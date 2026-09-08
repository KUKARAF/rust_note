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
