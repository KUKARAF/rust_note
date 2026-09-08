// Extension bundle for the note editor.
//
// This is upstream's `basicSetup` array literal (the `codemirror` package's
// docs explicitly tell you to copy it once you need to configure it, since it
// takes no options) with four deliberate deviations:
//
//   * `EditorView.lineWrapping` — long lines soft-wrap instead of scrolling
//     horizontally. Prose paragraphs are the normal case in notes, and the
//     Android build has no practical way to scroll a nested horizontal axis.
//   * `lineNumbers()` replaced by `lineNumbersRight()` — vim's `wrap` +
//     `number` behaviour: a number per *logical* line (wrapped continuation
//     rows stay unnumbered), rendered after the content instead of before it.
//   * `foldGutter()` dropped — with the numbers moved to the right it would be
//     the sole occupant of the left gutter, i.e. a permanently empty column
//     (markdown exposes almost no foldable ranges). `foldKeymap` is kept, so
//     keyboard folding still works where a fold range does exist.
//   * `lintKeymap` dropped — nothing in this app provides diagnostics, so its
//     bindings could only ever open an empty lint panel.
import {
	crosshairCursor,
	drawSelection,
	dropCursor,
	EditorView,
	gutter,
	GutterMarker,
	highlightActiveLine,
	highlightActiveLineGutter,
	highlightSpecialChars,
	keymap,
	rectangularSelection,
	type ViewUpdate
} from '@codemirror/view';
import { EditorState, type Extension, type RangeValue } from '@codemirror/state';
import {
	bracketMatching,
	defaultHighlightStyle,
	foldKeymap,
	indentOnInput,
	syntaxHighlighting
} from '@codemirror/language';
import { defaultKeymap, history, historyKeymap } from '@codemirror/commands';
import { highlightSelectionMatches, searchKeymap } from '@codemirror/search';
import {
	autocompletion,
	closeBrackets,
	closeBracketsKeymap,
	completionKeymap
} from '@codemirror/autocomplete';

/** A single rendered line number. */
class LineNumberMarker extends GutterMarker {
	constructor(readonly text: string) {
		super();
	}

	eq(other: RangeValue): boolean {
		return other instanceof LineNumberMarker && other.text === this.text;
	}

	toDOM(): Node {
		return document.createTextNode(this.text);
	}
}

/**
 * Widest number the gutter must be able to hold, rounded up to the next
 * all-nines value (9, 99, 999, …). Upstream's `lineNumbers` sizes its spacer
 * the same way: the gutter only changes width when the line count crosses a
 * power of ten, instead of re-measuring as the document grows.
 */
function spacerFor(lines: number): LineNumberMarker {
	let widest = 9;
	while (widest < lines) widest = widest * 10 + 9;
	return new LineNumberMarker(String(widest));
}

/**
 * Line numbers in a gutter placed *after* the content (`side: 'after'`, native
 * in @codemirror/view >= 6.43).
 *
 * The gutter renders one element per line block rather than per visual row, so
 * a soft-wrapped line gets exactly one number, aligned with its first row —
 * which is the whole point: a number marks a real newline, never a wrap.
 */
const lineNumbersRight: Extension = gutter({
	// Reuse upstream's class so the base theme's numeric alignment applies.
	class: 'cm-lineNumbers',
	side: 'after',
	renderEmptyElements: false,
	lineMarker: (view: EditorView, line) =>
		new LineNumberMarker(String(view.state.doc.lineAt(line.from).number)),
	initialSpacer: (view: EditorView) => spacerFor(view.state.doc.lines),
	updateSpacer: (current: GutterMarker, update: ViewUpdate) => {
		const next = spacerFor(update.view.state.doc.lines);
		return next.eq(current) ? current : next;
	}
});

/** Editor extensions shared by every note surface (own note, share link). */
export const noteSetup: Extension = [
	lineNumbersRight,
	highlightActiveLineGutter(),
	highlightSpecialChars(),
	history(),
	drawSelection(),
	dropCursor(),
	EditorState.allowMultipleSelections.of(true),
	indentOnInput(),
	syntaxHighlighting(defaultHighlightStyle, { fallback: true }),
	bracketMatching(),
	closeBrackets(),
	autocompletion(),
	rectangularSelection(),
	crosshairCursor(),
	highlightActiveLine(),
	highlightSelectionMatches(),
	EditorView.lineWrapping,
	keymap.of([
		...closeBracketsKeymap,
		...defaultKeymap,
		...searchKeymap,
		...historyKeymap,
		...foldKeymap,
		...completionKeymap
	])
];
