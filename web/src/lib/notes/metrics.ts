// Structured frontmatter metric tracking for daily notes.
//
// The chosen on-disk format (user decision, matches "proper YAML" over the
// vault's historical flat keys) is a list of entries per metric, each with a
// numeric `value` and an OPTIONAL quoted `time`:
//
//   ---
//   plan: true
//   calories_eaten:
//     - value: 300
//       time: "09:30"
//     - value: 500
//   ---
//
// This module is a deliberately minimal, PURE text-in/text-out YAML *subset*
// handler — not a general YAML library. It understands exactly:
//   * the leading `---` frontmatter fence block,
//   * flat `key: scalar` lines,
//   * `key:` followed by indented `- value: N` / `time: "HH:MM"` entry pairs.
// Everything it does not understand is preserved byte-for-byte and never
// rewritten: edits replace only the lines of the one metric being touched
// (or append before the closing fence). The server treats notes as opaque
// text and the collab CRDT syncs plain text, so these edits ride through
// exactly like typed keystrokes.

export interface MetricEntry {
	value: number;
	/** "HH:MM" — omitted for untimed entries. */
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
	// Find the closing fence: a line that is exactly `---`.
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

/** Normalize a metric name to the vault's key style: `calories eaten` → `calories_eaten`. */
export function normalizeMetricName(name: string): string {
	return name.trim().toLowerCase().replace(/\s+/g, '_');
}

function parseEntryValue(raw: string): number | null {
	const n = Number(raw.trim());
	return Number.isFinite(n) ? n : null;
}

function unquote(raw: string): string {
	const t = raw.trim();
	if (t.length >= 2 && t.startsWith('"') && t.endsWith('"')) return t.slice(1, -1);
	return t;
}

/**
 * Read all metrics we can understand from the doc's frontmatter. A flat
 * numeric scalar (`pomodoro: 3`) reads as a single untimed entry, so
 * existing vault keys show up in suggestions and can be migrated on write.
 * Non-numeric scalars (`plan: true`) and unknown structures are skipped.
 */
export function readMetrics(docText: string): Map<string, MetricEntry[]> {
	const metrics = new Map<string, MetricEntry[]>();
	const block = parseFrontmatterBlock(docText);
	if (block === null) return metrics;

	const lines = block.lines;
	let i = 0;
	while (i < lines.length) {
		const line = lines[i] ?? '';
		const keyMatch = /^([A-Za-z0-9_@-]+):(.*)$/.exec(line);
		if (keyMatch === null) {
			i += 1;
			continue;
		}
		const key = keyMatch[1] ?? '';
		const inline = (keyMatch[2] ?? '').trim();

		if (inline !== '') {
			const value = parseEntryValue(inline);
			if (value !== null) metrics.set(key, [{ value }]);
			i += 1;
			continue;
		}

		// `key:` with no inline value — collect indented `- value:` entries.
		const entries: MetricEntry[] = [];
		let j = i + 1;
		while (j < lines.length) {
			const entryLine = lines[j] ?? '';
			const valueMatch = /^\s+-\s+value:\s*(.+)$/.exec(entryLine);
			if (valueMatch === null) break;
			const value = parseEntryValue(valueMatch[1] ?? '');
			if (value === null) break;
			const entry: MetricEntry = { value };
			const timeLine = lines[j + 1] ?? '';
			const timeMatch = /^\s+time:\s*(.+)$/.exec(timeLine);
			if (timeMatch !== null) {
				entry.time = unquote(timeMatch[1] ?? '');
				j += 1;
			}
			entries.push(entry);
			j += 1;
		}
		if (entries.length > 0) metrics.set(key, entries);
		i = Math.max(j, i + 1);
	}
	return metrics;
}

function renderEntry(entry: MetricEntry): string[] {
	const lines = [`  - value: ${entry.value}`];
	// `time` contains ':' so YAML requires quoting.
	if (entry.time !== undefined && entry.time !== '') lines.push(`    time: "${entry.time}"`);
	return lines;
}

/**
 * Compute the text edit that appends one metric entry, returning the new
 * FULL document text. The caller applies it to the collab doc by replacing
 * the frontmatter region (delete + insert in one Yjs transaction).
 *
 * Cases handled:
 *  * no frontmatter at all → a new block is created at the top;
 *  * metric absent → `metric:` + entry appended before the closing fence;
 *  * metric present as a numeric scalar → migrated in place to list form
 *    (the scalar becomes the first, untimed entry) then the new entry added;
 *  * metric present as a list → entry appended at the end of that list.
 * Returns null when the frontmatter exists but is malformed (unclosed
 * fence) — we refuse to touch it rather than corrupt the note.
 */
export function appendMetricEntry(
	docText: string,
	rawName: string,
	value: number,
	time?: string
): string | null {
	const metric = normalizeMetricName(rawName);
	if (metric === '') return null;
	const entry: MetricEntry = time !== undefined && time !== '' ? { value, time } : { value };

	if (!docText.startsWith('---')) {
		// No frontmatter: create the whole block above the existing body.
		const blockLines = ['---', `${metric}:`, ...renderEntry(entry), '---'];
		return `${blockLines.join('\n')}\n${docText}`;
	}

	const block = parseFrontmatterBlock(docText);
	if (block === null) return null;

	const lines = [...block.lines];
	// Find the metric's key line and the extent of its existing entry list.
	let keyIndex = -1;
	let inlineScalar: string | null = null;
	for (let i = 0; i < lines.length; i += 1) {
		const m = new RegExp(`^${metric.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}:(.*)$`).exec(
			lines[i] ?? ''
		);
		if (m !== null) {
			keyIndex = i;
			const inline = (m[1] ?? '').trim();
			inlineScalar = inline === '' ? null : inline;
			break;
		}
	}

	if (keyIndex === -1) {
		// Absent: append at the end of the block.
		lines.push(`${metric}:`, ...renderEntry(entry));
	} else if (inlineScalar !== null) {
		// Scalar form: migrate to a list, keeping the old value as the first
		// (untimed) entry — only when it's numeric; a non-numeric scalar
		// (e.g. `plan: true`) is not a metric and we refuse to convert it.
		const oldValue = parseEntryValue(inlineScalar);
		if (oldValue === null) return null;
		lines.splice(
			keyIndex,
			1,
			`${metric}:`,
			...renderEntry({ value: oldValue }),
			...renderEntry(entry)
		);
	} else {
		// List form: skip past the existing entries, insert after the last.
		let end = keyIndex + 1;
		while (end < lines.length) {
			const l = lines[end] ?? '';
			if (/^\s+-\s+value:/.test(l) || /^\s+time:/.test(l)) {
				end += 1;
			} else {
				break;
			}
		}
		lines.splice(end, 0, ...renderEntry(entry));
	}

	const before = docText.slice(0, block.contentStart);
	const after = docText.slice(block.fenceStart);
	const middle = lines.length === 0 ? '' : `${lines.join('\n')}\n`;
	return `${before}${middle}${after}`;
}
