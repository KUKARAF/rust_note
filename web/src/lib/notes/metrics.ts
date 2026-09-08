// Structured frontmatter metric tracking for daily notes.
//
// Canonical on-disk format is the compact `@HHMM`-inline form (matches
// crates/core/src/stats.rs and the /stats feature):
//
//   ---
//   protein: 60
//   caffeine: [40@0720, 30@1500]
//   exercise.cardio: 30@0930
//   ---
//
// A single sample is a scalar (`caffeine: 40@0720`); repeats become an inline
// list. Time is `@HHMM` (24h, no colon). Keys may be dotted (`exercise.cardio`).
//
// This module is a deliberately minimal, PURE text-in/text-out YAML *subset*
// handler — not a general YAML library. Everything it does not understand is
// preserved byte-for-byte; edits replace only the one metric's line(s) (or
// append before the closing fence). The collab CRDT syncs plain text, so these
// edits ride through like typed keystrokes.
//
// It ALSO reads the older block-list form (`key:` + `- value: N` /
// `time: "HH:MM"`) so existing vault data still shows up; such a metric is
// rewritten to the inline form on its next `appendMetricEntry`.

export interface MetricEntry {
	value: number;
	/** "HH:MM" (display form) — omitted for untimed entries. */
	time?: string;
}

interface FrontmatterBlock {
	/** Char offset of the first content line (after the opening fence). */
	contentStart: number;
	/** Char offset of the closing `---` line's start. */
	fenceStart: number;
	/** The raw lines between the fences (no trailing newline). */
	lines: string[];
}

/**
 * Locate the frontmatter block at the very start of the doc. Returns null
 * when the doc doesn't start with a `---` fence or the closing fence is
 * missing (malformed — we then refuse to edit rather than guess).
 */
export function parseFrontmatterBlock(docText: string): FrontmatterBlock | null {
	if (!docText.startsWith('---\n') && docText !== '---') return null;
	const contentStart = 4;
	let offset = contentStart;
	while (offset <= docText.length) {
		const lineEnd = docText.indexOf('\n', offset);
		const line = lineEnd === -1 ? docText.slice(offset) : docText.slice(offset, lineEnd);
		if (line === '---') {
			const lines =
				offset === contentStart ? [] : docText.slice(contentStart, offset - 1).split('\n');
			return { contentStart, fenceStart: offset, lines };
		}
		if (lineEnd === -1) break;
		offset = lineEnd + 1;
	}
	return null;
}

/** Normalize a metric name to the vault's key style: `calories eaten` → `calories_eaten`. Dots are preserved for namespacing (`exercise.cardio`). */
export function normalizeMetricName(name: string): string {
	return name.trim().toLowerCase().replace(/\s+/g, '_');
}

/** Normalize an `HHMM` or `HH:MM` time token to `"HH:MM"`, or null if invalid. */
function normalizeTime(raw: string): string | null {
	const s = raw.trim().replace(/^"|"$/g, '');
	let hh: string;
	let mm: string;
	if (s.includes(':')) {
		const parts = s.split(':');
		hh = parts[0] ?? '';
		mm = parts[1] ?? '';
	} else if (/^\d{4}$/.test(s)) {
		hh = s.slice(0, 2);
		mm = s.slice(2);
	} else {
		return null;
	}
	const h = Number(hh);
	const m = Number(mm);
	if (!Number.isInteger(h) || !Number.isInteger(m) || h < 0 || h > 23 || m < 0 || m > 59)
		return null;
	if (mm.length !== 2) return null;
	return `${String(h).padStart(2, '0')}:${String(m).padStart(2, '0')}`;
}

/** Parse one inline numeric token: `N` or `N@HHMM`. */
function parsePoint(tok: string): MetricEntry | null {
	const t = tok.trim();
	const at = t.indexOf('@');
	if (at >= 0) {
		const value = Number(t.slice(0, at).trim());
		const time = normalizeTime(t.slice(at + 1));
		if (!Number.isInteger(value) || time === null) return null;
		return { value, time };
	}
	const value = Number(t);
	return Number.isInteger(value) ? { value } : null;
}

/** Parse an inline value (`N`, `N@HHMM`, or `[…]`). Returns null for bools/strings. */
function parseInlineValue(raw: string): MetricEntry[] | null {
	const t = raw.trim();
	if (t === '' || t === 'true' || t === 'false') return null;
	if (t.startsWith('[') && t.endsWith(']')) {
		const out: MetricEntry[] = [];
		for (const part of t.slice(1, -1).split(',')) {
			if (part.trim() === '') continue;
			const p = parsePoint(part);
			if (p === null) return null;
			out.push(p);
		}
		return out.length > 0 ? out : null;
	}
	const p = parsePoint(t);
	return p ? [p] : null;
}

function unquote(raw: string): string {
	const t = raw.trim();
	if (t.length >= 2 && t.startsWith('"') && t.endsWith('"')) return t.slice(1, -1);
	return t;
}

const KEY_RE = /^([A-Za-z0-9_@.-]+):(.*)$/;

/**
 * Read all metrics we can understand from the doc's frontmatter — the inline
 * form and the legacy block-list form. Non-numeric scalars (`plan: true`) and
 * unknown structures are skipped.
 */
export function readMetrics(docText: string): Map<string, MetricEntry[]> {
	const metrics = new Map<string, MetricEntry[]>();
	const block = parseFrontmatterBlock(docText);
	if (block === null) return metrics;

	const lines = block.lines;
	let i = 0;
	while (i < lines.length) {
		const line = lines[i] ?? '';
		const keyMatch = KEY_RE.exec(line);
		if (keyMatch === null) {
			i += 1;
			continue;
		}
		const key = keyMatch[1] ?? '';
		const inline = (keyMatch[2] ?? '').trim();

		if (inline !== '') {
			const entries = parseInlineValue(inline);
			if (entries !== null) metrics.set(key, entries);
			i += 1;
			continue;
		}

		// `key:` with no inline value — collect legacy `- value:` entries.
		const entries: MetricEntry[] = [];
		let j = i + 1;
		while (j < lines.length) {
			const valueMatch = /^\s+-\s+value:\s*(.+)$/.exec(lines[j] ?? '');
			if (valueMatch === null) break;
			const value = Number((valueMatch[1] ?? '').trim());
			if (!Number.isInteger(value)) break;
			const entry: MetricEntry = { value };
			const timeMatch = /^\s+time:\s*(.+)$/.exec(lines[j + 1] ?? '');
			if (timeMatch !== null) {
				const t = normalizeTime(unquote(timeMatch[1] ?? ''));
				if (t !== null) {
					entry.time = t;
					j += 1;
				}
			}
			entries.push(entry);
			j += 1;
		}
		if (entries.length > 0) metrics.set(key, entries);
		i = Math.max(j, i + 1);
	}
	return metrics;
}

/** Render one entry inline: `N` or `N@HHMM`. */
function renderPoint(entry: MetricEntry): string {
	return entry.time ? `${entry.value}@${entry.time.replace(':', '')}` : String(entry.value);
}

/** Render a `key: value` line (scalar when single, inline list otherwise). */
function renderLine(metric: string, entries: MetricEntry[]): string {
	if (entries.length === 1) return `${metric}: ${renderPoint(entries[0] as MetricEntry)}`;
	return `${metric}: [${entries.map(renderPoint).join(', ')}]`;
}

/** Collect the existing entries for a key line at `keyIndex`, plus the index
 * just past the key's lines (handles inline scalar/list and legacy block). */
function collectExisting(
	lines: string[],
	keyIndex: number
): { entries: MetricEntry[]; end: number } | null {
	const line = lines[keyIndex] ?? '';
	const inline = (KEY_RE.exec(line)?.[2] ?? '').trim();
	if (inline !== '') {
		const entries = parseInlineValue(inline);
		if (entries === null) return null; // bool/unparseable — refuse
		return { entries, end: keyIndex + 1 };
	}
	const entries: MetricEntry[] = [];
	let j = keyIndex + 1;
	while (j < lines.length) {
		const valueMatch = /^\s+-\s+value:\s*(.+)$/.exec(lines[j] ?? '');
		if (valueMatch === null) break;
		const value = Number((valueMatch[1] ?? '').trim());
		if (!Number.isInteger(value)) break;
		const entry: MetricEntry = { value };
		const timeMatch = /^\s+time:\s*(.+)$/.exec(lines[j + 1] ?? '');
		if (timeMatch !== null) {
			const t = normalizeTime(unquote(timeMatch[1] ?? ''));
			if (t !== null) {
				entry.time = t;
				j += 1;
			}
		}
		entries.push(entry);
		j += 1;
	}
	return { entries, end: j };
}

/**
 * Compute the text edit that appends one metric entry, returning the new FULL
 * document text (or null when the frontmatter is malformed or the key holds a
 * non-numeric value). The caller applies it to the collab doc by replacing the
 * frontmatter region in one Yjs transaction. Always writes the inline form.
 */
export function appendMetricEntry(
	docText: string,
	rawName: string,
	value: number,
	time?: string
): string | null {
	const metric = normalizeMetricName(rawName);
	if (metric === '' || !Number.isInteger(value)) return null;
	const normTime = time !== undefined && time !== '' ? normalizeTime(time) : null;
	if (time !== undefined && time !== '' && normTime === null) return null;
	const entry: MetricEntry = normTime !== null ? { value, time: normTime } : { value };

	if (!docText.startsWith('---')) {
		return `---\n${renderLine(metric, [entry])}\n---\n${docText}`;
	}

	const block = parseFrontmatterBlock(docText);
	if (block === null) return null;

	const lines = [...block.lines];
	const escaped = metric.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
	const keyRe = new RegExp(`^${escaped}:(.*)$`);
	const keyIndex = lines.findIndex((l) => keyRe.test(l));

	if (keyIndex === -1) {
		lines.push(renderLine(metric, [entry]));
	} else {
		const existing = collectExisting(lines, keyIndex);
		if (existing === null) return null;
		const entries = [...existing.entries, entry];
		lines.splice(keyIndex, existing.end - keyIndex, renderLine(metric, entries));
	}

	const before = docText.slice(0, block.contentStart);
	const after = docText.slice(block.fenceStart);
	const middle = lines.length === 0 ? '' : `${lines.join('\n')}\n`;
	return `${before}${middle}${after}`;
}
