// Aggregated-todo types plus the pure filter/sort/group logic shared by BOTH
// the manual sort/filter buttons and the AI natural-language query on the
// `/todo` board, so the two paths can never diverge. Mirrors the shape the
// server's `GET /api/todos` returns (see crates/core/src/tasks.rs).

export type Burner = 'frontburner' | 'backburner' | 'fridge' | 'oven';

/** One parsed task line plus the daily note it came from. */
export interface Todo {
	note_id: string;
	/** `YYYY-MM-DD` for a daily note, else null. */
	date: string | null;
	line: number;
	depth: number;
	marker: string;
	done: boolean;
	text: string;
	text_clean: string;
	pomodoros: number | null;
	start: string | null;
	due: string | null;
	tags: string[];
	burner: Burner | null;
}

export type SortField = 'burner' | 'pomodoros' | 'date' | 'start' | 'due';
export type SortDir = 'asc' | 'desc';
export interface SortKey {
	field: SortField;
	dir: SortDir;
}

export type StatusFilter = 'open' | 'done' | 'all';

/**
 * A structured query the LLM produces and the buttons build — always applied
 * client-side by [`applyQuery`]. Every field is optional so an empty spec is
 * "show everything, default order".
 */
export interface QuerySpec {
	/** Case-insensitive substring match against the task text. */
	text?: string;
	/** Keep only these burners (empty/omitted = all). */
	burners?: Burner[];
	/** Keep only tasks carrying ALL of these tags (without the leading `#`). */
	tags?: string[];
	/** open / done / all (default all — the board shows done dimmed). */
	status?: StatusFilter;
	pomodorosMin?: number;
	pomodorosMax?: number;
	/** Sort keys applied in order; the first is primary. */
	sort?: SortKey[];
}

/** Fixed display order of the burner groups. */
export const BURNER_ORDER: Burner[] = ['frontburner', 'backburner', 'fridge', 'oven'];

export interface BurnerMeta {
	label: string;
	/** Chip/accent color name understood by the design `Chip` component. */
	color: 'danger' | 'orange' | 'accent' | 'dim';
	glyph: string;
	hint: string;
}

export const BURNER_META: Record<Burner | 'other', BurnerMeta> = {
	frontburner: { label: 'Frontburner', color: 'danger', glyph: '●', hint: 'do now' },
	backburner: { label: 'Backburner', color: 'orange', glyph: '◐', hint: 'simmering' },
	fridge: { label: 'Fridge', color: 'accent', glyph: '❄', hint: 'before it spoils' },
	oven: { label: 'Oven', color: 'dim', glyph: '○', hint: 'eventually' },
	other: { label: 'Unsorted', color: 'dim', glyph: '·', hint: 'no burner tag' }
};

/**
 * Days past a fridge item's note date at which it's considered "spoiling" and
 * gets surfaced/flagged. A tunable knob for the `#fridge` "needs attention
 * before it spoils" behaviour.
 */
export const FRIDGE_SPOILS_AFTER_DAYS = 14;

/** Whole days between a `YYYY-MM-DD` note date and `now` (0 if no/invalid date). */
export function ageInDays(date: string | null, now: Date): number {
	if (!date) return 0;
	const then = Date.parse(`${date}T00:00:00`);
	if (Number.isNaN(then)) return 0;
	const start = new Date(now.getFullYear(), now.getMonth(), now.getDate()).getTime();
	return Math.max(0, Math.round((start - then) / 86_400_000));
}

/** A fridge task old enough to need attention. */
export function isSpoiling(todo: Todo, now: Date): boolean {
	return (
		todo.burner === 'fridge' &&
		!todo.done &&
		ageInDays(todo.date, now) >= FRIDGE_SPOILS_AFTER_DAYS
	);
}

function matchesFilters(todo: Todo, spec: QuerySpec): boolean {
	const status = spec.status ?? 'all';
	if (status === 'open' && todo.done) return false;
	if (status === 'done' && !todo.done) return false;

	if (spec.text && spec.text.trim() !== '') {
		if (!todo.text_clean.toLowerCase().includes(spec.text.toLowerCase())) return false;
	}
	if (spec.burners && spec.burners.length > 0) {
		if (!todo.burner || !spec.burners.includes(todo.burner)) return false;
	}
	if (spec.tags && spec.tags.length > 0) {
		const have = new Set(todo.tags.map((t) => t.toLowerCase()));
		if (!spec.tags.every((t) => have.has(t.toLowerCase()))) return false;
	}
	if (spec.pomodorosMin != null && (todo.pomodoros ?? 0) < spec.pomodorosMin) return false;
	if (spec.pomodorosMax != null && (todo.pomodoros ?? Infinity) > spec.pomodorosMax) return false;
	return true;
}

const BURNER_RANK: Record<Burner, number> = {
	frontburner: 0,
	backburner: 1,
	fridge: 2,
	oven: 3
};

/** Compare two todos by one sort key. Nulls always sort last. */
function compareBy(a: Todo, b: Todo, key: SortKey): number {
	const dir = key.dir === 'desc' ? -1 : 1;
	const nullsLast = (x: number | null, y: number | null): number | null => {
		if (x == null && y == null) return 0;
		if (x == null) return 1; // a after b regardless of dir
		if (y == null) return -1;
		return null; // both present — caller compares
	};

	switch (key.field) {
		case 'burner': {
			const av = a.burner ? BURNER_RANK[a.burner] : 99;
			const bv = b.burner ? BURNER_RANK[b.burner] : 99;
			return (av - bv) * dir;
		}
		case 'pomodoros': {
			const forced = nullsLast(a.pomodoros, b.pomodoros);
			if (forced != null) return forced;
			return ((a.pomodoros as number) - (b.pomodoros as number)) * dir;
		}
		case 'date': {
			const av = a.date ?? '';
			const bv = b.date ?? '';
			if (av === bv) return 0;
			return (av < bv ? -1 : 1) * dir;
		}
		case 'start':
		case 'due': {
			const av = a[key.field];
			const bv = b[key.field];
			if (!av && !bv) return 0;
			if (!av) return 1;
			if (!bv) return -1;
			if (av === bv) return 0;
			return (av < bv ? -1 : 1) * dir;
		}
	}
}

/** Default within-group ordering when the user hasn't chosen a sort. */
function defaultSort(burner: Burner | 'other', now: Date): (a: Todo, b: Todo) => number {
	return (a, b) => {
		// Open before done, everywhere.
		if (a.done !== b.done) return a.done ? 1 : -1;
		// Fridge: oldest (most-spoiled) first so stale items surface at the top.
		if (burner === 'fridge') {
			const byAge = ageInDays(b.date, now) - ageInDays(a.date, now);
			if (byAge !== 0) return byAge;
		}
		// Otherwise most-recent day first, then earlier start time.
		const byDate = (b.date ?? '').localeCompare(a.date ?? '');
		if (byDate !== 0) return byDate;
		return (a.start ?? '~').localeCompare(b.start ?? '~');
	};
}

function sortWithin(
	todos: Todo[],
	burner: Burner | 'other',
	sort: SortKey[] | undefined,
	now: Date
): Todo[] {
	const out = [...todos];
	if (sort && sort.length > 0) {
		out.sort((a, b) => {
			// Open before done regardless of the chosen key.
			if (a.done !== b.done) return a.done ? 1 : -1;
			for (const key of sort) {
				const c = compareBy(a, b, key);
				if (c !== 0) return c;
			}
			return 0;
		});
	} else {
		out.sort(defaultSort(burner, now));
	}
	return out;
}

export interface BurnerGroup {
	burner: Burner | 'other';
	meta: BurnerMeta;
	todos: Todo[];
	/** Count of open (not-done) tasks in the group. */
	openCount: number;
}

export interface QueryResult {
	groups: BurnerGroup[];
	/** Total tasks after filtering (open + done). */
	total: number;
	/** Total open tasks after filtering. */
	openTotal: number;
}

/**
 * Apply `spec` to `todos`: filter, then group by burner in [`BURNER_ORDER`]
 * (untagged → "other" last), sorting within each group. Empty groups are
 * dropped. Pure — pass `now` for deterministic fridge-aging.
 */
export function applyQuery(todos: Todo[], spec: QuerySpec, now: Date): QueryResult {
	const kept = todos.filter((t) => matchesFilters(t, spec));

	const buckets = new Map<Burner | 'other', Todo[]>();
	for (const t of kept) {
		const key = t.burner ?? 'other';
		const arr = buckets.get(key) ?? [];
		arr.push(t);
		buckets.set(key, arr);
	}

	const order: (Burner | 'other')[] = [...BURNER_ORDER, 'other'];
	const groups: BurnerGroup[] = [];
	for (const burner of order) {
		const items = buckets.get(burner);
		if (!items || items.length === 0) continue;
		const sorted = sortWithin(items, burner, spec.sort, now);
		groups.push({
			burner,
			meta: BURNER_META[burner],
			todos: sorted,
			openCount: sorted.filter((t) => !t.done).length
		});
	}

	return {
		groups,
		total: kept.length,
		openTotal: kept.filter((t) => !t.done).length
	};
}
