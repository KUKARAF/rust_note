// Pipeline-tracker types plus the pure filter/sort/group helpers used by the
// `/pipeline` board. Mirrors `GET /api/pipeline` (see docs/pipeline.md).
import { apiGet } from '$lib/api/client';

export type Kind = 'lead' | 'application';
export type Ball = 'ours' | 'theirs' | 'none';
export type KindFilter = 'all' | Kind;

export interface PipelineItem {
	id: string;
	kind: Kind;
	company: string;
	role: string | null;
	stage: string | null;
	ball: Ball;
	/** RFC3339 or null. */
	expected_at: string | null;
	next_action: string | null;
	source: string | null;
	url: string | null;
	contract: string | null;
	rate_asked: string | null;
	rate_offered: string | null;
	contacts: string[];
	tags: string[];
	/** 1-5 (5 = highest) or null. */
	priority: number | null;
	applied_at: string | null;
	last_activity_at: string | null;
	next_interview_at: string | null;
	closed_reason: string | null;
	/** Server-derived: expected_at is in the past. */
	overdue: boolean;
}

export const TERMINAL_STAGES = ['closed', 'won', 'lost', 'rejected', 'withdrawn', 'accepted'];

/** Mirrors `PipelineItem::is_closed`: terminal stage OR ball == none. */
export function isClosed(item: PipelineItem): boolean {
	return item.ball === 'none' || TERMINAL_STAGES.includes((item.stage ?? '').toLowerCase());
}

/** `GET /api/pipeline?kind=…&include_closed=…`. */
export function fetchPipeline(kind: KindFilter, includeClosed: boolean): Promise<PipelineItem[]> {
	const params = new URLSearchParams();
	if (kind !== 'all') params.set('kind', kind);
	if (includeClosed) params.set('include_closed', 'true');
	const qs = params.toString();
	return apiGet<PipelineItem[]>(`/api/pipeline${qs ? `?${qs}` : ''}`);
}

function expectedMs(item: PipelineItem): number | null {
	if (!item.expected_at) return null;
	const ms = Date.parse(item.expected_at);
	return Number.isNaN(ms) ? null : ms;
}

/** Items the ball is in play for and that are past `expected_at`, most overdue first. */
export function needsAttention(items: PipelineItem[]): PipelineItem[] {
	return items
		.filter((i) => i.ball !== 'none' && i.overdue)
		.sort((a, b) => (expectedMs(a) ?? Infinity) - (expectedMs(b) ?? Infinity));
}

/** Priority desc (null last), then expected_at asc (null last). */
export function comparePipeline(a: PipelineItem, b: PipelineItem): number {
	const pa = a.priority ?? 0;
	const pb = b.priority ?? 0;
	if (pa !== pb) return pb - pa;
	return (expectedMs(a) ?? Infinity) - (expectedMs(b) ?? Infinity);
}

export interface StageGroup {
	stage: string;
	items: PipelineItem[];
}

/** Group by stage, sorted within each group; groups ordered by the funnel vocabulary. */
export function groupByStage(items: PipelineItem[]): StageGroup[] {
	const order = ['prospect', 'applied', 'talking', 'screening', 'interview', 'proposal', 'offer'];
	const map = new Map<string, PipelineItem[]>();
	for (const i of items) {
		const key = (i.stage ?? 'no stage').toLowerCase();
		const list = map.get(key);
		if (list) list.push(i);
		else map.set(key, [i]);
	}
	const rank = (s: string) => {
		const idx = order.indexOf(s);
		return idx === -1 ? order.length : idx;
	};
	return [...map.entries()]
		.sort(([a], [b]) => rank(a) - rank(b) || a.localeCompare(b))
		.map(([stage, list]) => ({ stage, items: list.sort(comparePipeline) }));
}

/** "2d overdue" / "in 3d" / "today"; empty when there is no (valid) date. */
export function relativeExpected(expectedAt: string | null, now: Date): string {
	if (!expectedAt) return '';
	const ms = Date.parse(expectedAt);
	if (Number.isNaN(ms)) return '';
	const diffMs = ms - now.getTime();
	const abs = Math.abs(diffMs);
	const hours = Math.floor(abs / 3_600_000);
	const days = Math.floor(abs / 86_400_000);
	const unit = days >= 1 ? `${days}d` : hours >= 1 ? `${hours}h` : '<1h';
	if (days === 0 && hours === 0) return diffMs < 0 ? 'just due' : 'soon';
	return diffMs < 0 ? `${unit} overdue` : `in ${unit}`;
}
