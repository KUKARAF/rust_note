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
	/** `@location` contexts (no leading `@`, lowercased). Empty when none. */
	locations: string[];
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
	/**
	 * Location contexts (without the leading `@`). A task is kept if it carries
	 * ANY of these locations — OR has no location at all (a location-less task is
	 * relevant in every context, so it stays visible when filtering by place).
	 */
	locations?: string[];
	/** open / done / all (default all — the board shows done dimmed). */
	status?: StatusFilter;
	pomodorosMin?: number;
	pomodorosMax?: number;
	/** Sort keys applied in order; the first is primary. */
	sort?: SortKey[];
}

/** Map a search token to a burner (short or long form), else null. */
function burnerFromToken(tok: string): Burner | null {
	switch (tok.toLowerCase()) {
		case 'fb':
		case 'frontburner':
			return 'frontburner';
		case 'bb':
		case 'backburner':
			return 'backburner';
		case 'fridge':
			return 'fridge';
		case 'oven':
			return 'oven';
		default:
			return null;
	}
}

/**
 * Parse the single search bar into filter fields, so power users can type
 * `fridge @home p>=2 export` instead of hunting through controls. Recognized
 * tokens: `#tag`, `@location`, a burner name (`fb`/`frontburner`/`bb`/
 * `backburner`/`fridge`/`oven`), and `p>=N` / `p<=N`. Everything else is free
 * text. Status and sort live in the Display popover, not here.
 */
export function parseSearch(query: string): QuerySpec {
	const spec: QuerySpec = {};
	const words: string[] = [];
	const tags: string[] = [];
	const locations: string[] = [];
	const burners: Burner[] = [];
	for (const raw of query.split(/\s+/)) {
		const tok = raw.trim();
		if (tok === '') continue;
		if (tok.length > 1 && tok.startsWith('#')) {
			tags.push(tok.slice(1).toLowerCase());
			continue;
		}
		if (tok.length > 1 && tok.startsWith('@')) {
			locations.push(tok.slice(1).toLowerCase());
			continue;
		}
		const b = burnerFromToken(tok);
		if (b) {
			if (!burners.includes(b)) burners.push(b);
			continue;
		}
		const ge = /^p>=(\d+)$/.exec(tok);
		if (ge) {
			spec.pomodorosMin = Number(ge[1]);
			continue;
		}
		const le = /^p<=(\d+)$/.exec(tok);
		if (le) {
			spec.pomodorosMax = Number(le[1]);
			continue;
		}
		words.push(tok);
	}
	if (words.length > 0) spec.text = words.join(' ');
	if (tags.length > 0) spec.tags = tags;
	if (locations.length > 0) spec.locations = locations;
	if (burners.length > 0) spec.burners = burners;
	return spec;
}

/** Fixed display order of the burner groups. */
export const BURNER_ORDER: Burner[] = ['frontburner', 'backburner', 'fridge', 'oven'];

/**
 * Priority color per burner, used for the checkbox ring and the group dot —
 * the ONLY place burner color appears now (rows are otherwise neutral). Green
 * (`accent`) is reserved for interaction, so fridge is blue ("cold/parked"),
 * not green.
 */
export const BURNER_COLOR: Record<Burner, 'danger' | 'orange' | 'blue' | 'dim'> = {
	frontburner: 'danger',
	backburner: 'orange',
	fridge: 'blue',
	oven: 'dim'
};

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
		todo.burner === 'fridge' && !todo.done && ageInDays(todo.date, now) >= FRIDGE_SPOILS_AFTER_DAYS
	);
}

/** Local `YYYY-MM-DD` for `now` (used for date grouping + relative labels). */
export function todayStr(now: Date): string {
	const y = now.getFullYear();
	const m = String(now.getMonth() + 1).padStart(2, '0');
	const d = String(now.getDate()).padStart(2, '0');
	return `${y}-${m}-${d}`;
}

/** Short, muted relative wording for a note date: "today", "yesterday", "3d ago", or the date for future days. */
export function relativeDate(date: string | null, now: Date): string {
	if (!date) return '';
	const today = todayStr(now);
	if (date === today) return 'today';
	if (date < today) {
		const age = ageInDays(date, now);
		return age === 1 ? 'yesterday' : `${age}d ago`;
	}
	return date;
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
	if (spec.locations && spec.locations.length > 0 && todo.locations.length > 0) {
		// Context filter: a placed task must match one of the wanted locations.
		// Location-less tasks intentionally skip this check (kept everywhere).
		const want = new Set(spec.locations.map((l) => l.toLowerCase()));
		if (!todo.locations.some((l) => want.has(l.toLowerCase()))) return false;
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

export type GroupMode = 'burner' | 'date';

/** A rendered section on the board — a quiet header (colored dot + label + count) over its tasks. */
export interface Group {
	/** Stable key: a burner name / 'other', or a date bucket ('overdue'…). */
	key: string;
	label: string;
	hint?: string;
	/** Dot color for the quiet header. */
	color: 'danger' | 'orange' | 'accent' | 'blue' | 'dim';
	todos: Todo[];
	/** Count of open (not-done) tasks in the group. */
	openCount: number;
}

export interface QueryResult {
	groups: Group[];
	/** Total tasks after filtering (open + done). */
	total: number;
	/** Total open tasks after filtering. */
	openTotal: number;
}

function groupByBurner(kept: Todo[], spec: QuerySpec, now: Date): Group[] {
	const buckets = new Map<Burner | 'other', Todo[]>();
	for (const t of kept) {
		const key = t.burner ?? 'other';
		const arr = buckets.get(key) ?? [];
		arr.push(t);
		buckets.set(key, arr);
	}
	const order: (Burner | 'other')[] = [...BURNER_ORDER, 'other'];
	const groups: Group[] = [];
	for (const burner of order) {
		const items = buckets.get(burner);
		if (!items || items.length === 0) continue;
		const meta = BURNER_META[burner];
		const sorted = sortWithin(items, burner, spec.sort, now);
		groups.push({
			key: burner,
			label: meta.label,
			hint: meta.hint,
			color: burner === 'other' ? 'dim' : BURNER_COLOR[burner],
			todos: sorted,
			openCount: sorted.filter((t) => !t.done).length
		});
	}
	return groups;
}

function groupByDate(kept: Todo[], spec: QuerySpec, now: Date): Group[] {
	const today = todayStr(now);
	const sections: { key: string; label: string; color: Group['color']; test: (t: Todo) => boolean }[] =
		[
			{ key: 'overdue', label: 'Overdue', color: 'danger', test: (t) => t.date != null && t.date < today },
			{ key: 'today', label: 'Today', color: 'accent', test: (t) => t.date === today },
			{ key: 'upcoming', label: 'Upcoming', color: 'dim', test: (t) => t.date != null && t.date > today },
			{ key: 'nodate', label: 'No date', color: 'dim', test: (t) => t.date == null }
		];
	const groups: Group[] = [];
	for (const s of sections) {
		const items = kept.filter(s.test);
		if (items.length === 0) continue;
		const sorted = sortWithin(items, 'other', spec.sort, now);
		groups.push({
			key: s.key,
			label: s.label,
			color: s.color,
			todos: sorted,
			openCount: sorted.filter((t) => !t.done).length
		});
	}
	return groups;
}

/**
 * Apply `spec` to `todos`: filter, then group (by burner in [`BURNER_ORDER`]
 * with "other" last, or by relative date), sorting within each group. Empty
 * groups are dropped. Pure — pass `now` for deterministic fridge-aging.
 */
export function applyQuery(
	todos: Todo[],
	spec: QuerySpec,
	now: Date,
	groupMode: GroupMode = 'burner'
): QueryResult {
	const kept = todos.filter((t) => matchesFilters(t, spec));
	const groups = groupMode === 'date' ? groupByDate(kept, spec, now) : groupByBurner(kept, spec, now);
	return {
		groups,
		total: kept.length,
		openTotal: kept.filter((t) => !t.done).length
	};
}
