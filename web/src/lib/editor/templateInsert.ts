// In-note `#template!` inserter.
//
// Mirrors the editor's other in-note trigger conventions: like `#AI!` the
// marker is a literal token the user types into the document, and like the
// floating "🔊 speak selection" button it surfaces as a CodeMirror
// `showTooltip` anchored right at that text (CM6 owns positioning/scrolling,
// far more reliable than a hand-placed overlay against the editor's own
// non-native selection).
//
// When `#template!` appears in the doc a small popup opens at it containing a
// text input wired to a `<datalist>` of every note under the vault's
// `templates/` folder. Picking a template replaces the marker in place with
// that template note's body (frontmatter stripped); Escape / clicking away
// dismisses without inserting.
//
// No backend change: the template list comes from the existing `GET /api/notes`
// (filtered to ids under `templates/`) and a template's body from
// `GET /api/notes/<id>`, both via the shared auth-carrying `apiGet` so it works
// identically in the website and the Tauri app.

import { showTooltip, type Tooltip } from '@codemirror/view';
import { EditorState, StateEffect, StateField, type Extension } from '@codemirror/state';
import { apiGet } from '$lib/api/client';
import { encodeNotePath } from '$lib/notes/path';
import { parseFrontmatterBlock } from '$lib/notes/metrics';

/** The literal token that, once fully typed into a note, opens the inserter. */
export const TEMPLATE_MARKER = '#template!';

/** Folder prefix (note-id form) that a note must live under to be a template. */
const TEMPLATES_PREFIX = 'templates/';

interface NoteMeta {
	id: string;
	title: string;
}

interface NoteResponse {
	content: string;
}

/**
 * Session cache of the template list. Fetched lazily the first time a popup
 * opens and reused afterwards so repeated triggers don't re-hit the network;
 * a failed fetch isn't cached (left null) so it can retry next time.
 */
let templatesCache: NoteMeta[] | null = null;

/** Strip a leading YAML frontmatter block (`---\n…\n---`) from a note body. */
export function stripFrontmatter(content: string): string {
	const block = parseFrontmatterBlock(content);
	if (!block) return content;
	// `fenceStart` is the offset of the closing `---`; skip it and the single
	// newline that follows (if any) so the body starts at real content.
	let after = block.fenceStart + '---'.length;
	if (content[after] === '\n') after += 1;
	return content.slice(after);
}

/** Load the template list (ids under `templates/`), using the session cache. */
async function loadTemplates(): Promise<NoteMeta[]> {
	if (templatesCache) return templatesCache;
	const notes = await apiGet<NoteMeta[]>('/api/notes');
	const templates = notes
		.filter((n) => n.id.startsWith(TEMPLATES_PREFIX) && n.id !== TEMPLATES_PREFIX)
		.sort((a, b) => a.id.localeCompare(b.id));
	templatesCache = templates;
	return templates;
}

/**
 * Resolve a user-typed value (from the input/datalist) to a template note id.
 * The datalist offers the short name (id without the `templates/` prefix) as
 * each option's value and the title as its label, so accept a match on the
 * short name, the full id, or the title — case-insensitively — to be forgiving
 * of what the browser commits back into the field.
 */
function resolveTemplateId(value: string, templates: NoteMeta[]): string | null {
	const v = value.trim().toLowerCase();
	if (v === '') return null;
	const match = templates.find((t) => {
		const short = t.id.slice(TEMPLATES_PREFIX.length);
		return t.id.toLowerCase() === v || short.toLowerCase() === v || t.title.toLowerCase() === v;
	});
	return match ? match.id : null;
}

/** Fetch a template note's body with frontmatter stripped. */
async function fetchTemplateBody(id: string): Promise<string> {
	const note = await apiGet<NoteResponse>(`/api/notes/${encodeNotePath(id)}`);
	return stripFrontmatter(note.content);
}

/** Locate the `#template!` marker nearest the cursor; null if none present. */
function findMarker(state: EditorState): { from: number; to: number } | null {
	const doc = state.doc.toString();
	const head = state.selection.main.head;
	let best: { from: number; to: number } | null = null;
	let bestDist = Infinity;
	let idx = doc.indexOf(TEMPLATE_MARKER);
	while (idx !== -1) {
		const from = idx;
		const to = idx + TEMPLATE_MARKER.length;
		// Distance from the cursor to the marker range (0 if inside it).
		const dist = head < from ? from - head : head > to ? head - to : 0;
		if (dist < bestDist) {
			bestDist = dist;
			best = { from, to };
		}
		idx = doc.indexOf(TEMPLATE_MARKER, idx + TEMPLATE_MARKER.length);
	}
	return best;
}

/**
 * Effect fired to dismiss the popup for the marker at the given offset without
 * inserting (Escape / click-away). The field remembers that offset and keeps
 * the popup closed until the document changes (which clears the memory, so a
 * freshly typed or moved marker re-opens it).
 */
const dismissMarker = StateEffect.define<number>();

interface TemplateState {
	tooltip: Tooltip | null;
	/** Marker `from` offset that was dismissed, or -1 if none. */
	dismissedAt: number;
}

/**
 * Build the `#template!` inserter extension. `onError`, when provided, is
 * called with a human-readable message if listing or fetching a template
 * fails (e.g. to surface a toast); the popup also shows it inline.
 */
export function templateInsertExtension(
	opts: { onError?: (message: string) => void } = {}
): Extension {
	function buildTooltip(marker: { from: number; to: number }): Tooltip {
		return {
			pos: marker.from,
			above: true,
			strictSide: true,
			arrow: false,
			create: (view) => {
				const dom = document.createElement('div');
				dom.className = 'cm-template-popup';

				const input = document.createElement('input');
				input.type = 'text';
				input.className = 'cm-template-input';
				input.placeholder = 'template name…';
				input.setAttribute('aria-label', 'Insert template');
				input.autocomplete = 'off';

				const listId = `cm-template-list-${Math.random().toString(36).slice(2)}`;
				const datalist = document.createElement('datalist');
				datalist.id = listId;
				input.setAttribute('list', listId);

				const status = document.createElement('span');
				status.className = 'cm-template-status';
				status.textContent = 'loading…';

				dom.append(input, datalist, status);

				let templates: NoteMeta[] = [];
				let busy = false;

				// Resolve the current marker range at action time (collab edits can
				// shift it between render and commit), falling back to the original.
				const currentMarker = () => findMarker(view.state) ?? marker;

				const close = () => {
					view.dispatch({ effects: dismissMarker.of(currentMarker().from) });
					view.focus();
				};

				const insert = async (id: string) => {
					if (busy) return;
					busy = true;
					status.textContent = 'inserting…';
					try {
						const body = await fetchTemplateBody(id);
						const m = currentMarker();
						view.dispatch({
							changes: { from: m.from, to: m.to, insert: body },
							selection: { anchor: m.from + body.length }
						});
						view.focus();
					} catch (err) {
						busy = false;
						const message = err instanceof Error ? err.message : 'Failed to load template';
						status.textContent = message;
						opts.onError?.(message);
					}
				};

				const tryCommit = () => {
					const id = resolveTemplateId(input.value, templates);
					if (id) void insert(id);
				};

				input.addEventListener('keydown', (e) => {
					if (e.key === 'Escape') {
						e.preventDefault();
						e.stopPropagation();
						close();
					} else if (e.key === 'Enter') {
						e.preventDefault();
						tryCommit();
					}
				});
				// Picking a datalist option fires `input` (and `change`); commit on
				// an exact match so a click/keyboard pick inserts immediately.
				input.addEventListener('input', tryCommit);
				input.addEventListener('change', tryCommit);
				// Click-away: dismiss once focus leaves the popup entirely. Deferred
				// so a datalist pick (which can briefly blur) still commits first.
				input.addEventListener('blur', () => {
					setTimeout(() => {
						if (!busy && !dom.contains(document.activeElement)) close();
					}, 150);
				});

				void loadTemplates().then(
					(list) => {
						templates = list;
						datalist.replaceChildren(
							...list.map((t) => {
								const opt = document.createElement('option');
								// Short name is the friendliest value to type/pick; the
								// title rides along as the option label.
								opt.value = t.id.slice(TEMPLATES_PREFIX.length);
								if (t.title && t.title !== opt.value) opt.label = t.title;
								return opt;
							})
						);
						status.textContent = list.length === 0 ? 'no templates' : `${list.length} templates`;
					},
					(err) => {
						const message = err instanceof Error ? err.message : 'Failed to load templates';
						status.textContent = message;
						opts.onError?.(message);
					}
				);

				return {
					dom,
					mount: () => {
						// Focus the input so the user can type/pick straight away.
						requestAnimationFrame(() => input.focus());
					}
				};
			}
		};
	}

	return StateField.define<TemplateState>({
		create(state) {
			const marker = findMarker(state);
			return { tooltip: marker ? buildTooltip(marker) : null, dismissedAt: -1 };
		},
		update(value, tr) {
			let dismissedAt = value.dismissedAt;
			// Any document edit invalidates a prior dismissal — a re-typed or
			// moved marker should re-open.
			if (tr.docChanged) dismissedAt = -1;
			for (const e of tr.effects) {
				if (e.is(dismissMarker)) dismissedAt = e.value;
			}
			// Nothing that could change the popup happened: keep the old state
			// (unless only the dismissal memory changed).
			if (!tr.docChanged && !tr.selection && dismissedAt === value.dismissedAt) {
				return value;
			}
			const marker = findMarker(tr.state);
			if (!marker || marker.from === dismissedAt) return { tooltip: null, dismissedAt };
			// Keep an open tooltip stable (same `pos`) across selection moves so it
			// doesn't flicker; only rebuild when the marker position changes.
			if (value.tooltip && value.tooltip.pos === marker.from && dismissedAt === -1) {
				return { tooltip: value.tooltip, dismissedAt };
			}
			return { tooltip: buildTooltip(marker), dismissedAt };
		},
		provide: (field) =>
			showTooltip.computeN([field], (state) => {
				const tt = state.field(field).tooltip;
				return tt ? [tt] : [];
			})
	});
}
