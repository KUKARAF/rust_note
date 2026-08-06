<script lang="ts">
	// CodeMirror 6 markdown editor.
	//
	// Phase 2: real-time collaboration. When `collab` handles are passed, the
	// editor binds directly to a shared Yjs `Y.Text` via y-codemirror.next's
	// `yCollab` — remote carets/selections render automatically and the CRDT
	// (not `value`) is the source of truth. Without `collab` it degrades to a
	// plain single-user editor bound to `value`.
	import { onMount, onDestroy } from 'svelte';
	import { EditorView, keymap } from '@codemirror/view';
	import { Compartment, EditorState, Prec, type Extension } from '@codemirror/state';
	import { markdown } from '@codemirror/lang-markdown';
	import { HighlightStyle, syntaxHighlighting } from '@codemirror/language';
	import { tags } from '@lezer/highlight';
	import { basicSetup } from 'codemirror';
	import { yCollab, yUndoManagerKeymap } from 'y-codemirror.next';
	import type * as Y from 'yjs';
	import type { Awareness } from 'y-protocols/awareness';

	interface CollabHandles {
		ytext: Y.Text;
		awareness: Awareness;
		undoManager: Y.UndoManager;
	}

	let {
		value = $bindable(''),
		onChange,
		onBlur,
		onSave,
		collab = null,
		editable = true,
		extensions = []
	}: {
		/** Current document content. Bindable: updated as the user types. */
		value?: string;
		/** Called with the new content whenever the document changes. */
		onChange?: (value: string) => void;
		/** Called when the editor loses focus. */
		onBlur?: () => void;
		/** Called when the user presses Ctrl+S / Cmd+S inside the editor. */
		onSave?: () => void;
		/**
		 * When provided, the editor collaborates via this shared Yjs text +
		 * awareness instead of the `value` binding. Must be stable for the
		 * lifetime of this component instance (the parent remounts on note change).
		 */
		collab?: CollabHandles | null;
		/**
		 * When false, the editor renders read-only: local edits are blocked but
		 * remote content/cursors still render and update live (used for
		 * view-only guest share links). Defaults to editable.
		 */
		editable?: boolean;
		/** Extra CodeMirror extensions to include. */
		extensions?: Extension[];
	} = $props();

	let host: HTMLDivElement;
	let view: EditorView | undefined;
	// Guards against feeding our own `dispatch`-triggered updates back into
	// `view` via the reactive `$effect` below.
	let updatingFromOutside = false;
	// Lets `editable` be toggled after mount (e.g. if a share link's
	// permission were ever re-resolved) without recreating the whole editor.
	const editableCompartment = new Compartment();

	function saveKeymap() {
		return keymap.of([
			{
				key: 'Mod-s',
				preventDefault: true,
				run: () => {
					onSave?.();
					return true;
				}
			}
		]);
	}

	// CRT-phosphor terminal theme for the CodeMirror chrome (background,
	// gutters, selection, cursor) — separate from the syntax highlighting
	// style below, since CodeMirror renders its own DOM and won't pick up
	// page CSS custom properties automatically for these internal parts.
	const kvEditorTheme = EditorView.theme(
		{
			'&': {
				backgroundColor: 'var(--surface-input)',
				color: 'var(--kv-ink)',
				height: '100%'
			},
			'.cm-content': {
				fontFamily: 'var(--font-term)',
				fontSize: 'var(--type-body)',
				lineHeight: 'var(--leading-term)',
				caretColor: 'var(--kv-accent)'
			},
			'.cm-scroller': {
				fontFamily: 'var(--font-term)',
				overflow: 'auto'
			},
			'&.cm-focused .cm-cursor': {
				borderLeftColor: 'var(--kv-accent)'
			},
			'&.cm-focused .cm-selectionBackground, .cm-selectionBackground, ::selection': {
				backgroundColor: 'rgba(121, 242, 121, 0.22) !important'
			},
			'.cm-gutters': {
				backgroundColor: 'var(--surface-input)',
				color: 'var(--kv-faint)',
				border: 'none',
				borderRight: '1px solid var(--border-default)'
			},
			'.cm-activeLineGutter': {
				backgroundColor: 'transparent',
				color: 'var(--kv-dim)'
			},
			'.cm-activeLine': {
				backgroundColor: 'rgba(121, 242, 121, 0.04)'
			},
			'.cm-line': {
				padding: '0 4px'
			}
		},
		{ dark: true }
	);

	// Remote-cursor styling for y-codemirror.next carets against the dark
	// RATION theme. The library sets each caret's inline `background-color`/
	// `border-color` to the peer's awareness color; here we just make the caret
	// bar solid and the hovering name-tag legible (light ink on the user color).
	const remoteCaretTheme = EditorView.theme({
		'.cm-ySelectionCaret': {
			borderLeftWidth: '2px',
			borderRightWidth: '0',
			marginLeft: '-1px',
			marginRight: '-1px'
		},
		'.cm-ySelectionCaretDot': {
			width: '.45em',
			height: '.45em',
			top: '-.25em'
		},
		'.cm-ySelectionInfo': {
			fontFamily: 'var(--font-term)',
			fontSize: 'var(--type-meta)',
			color: 'var(--kv-bg)',
			fontWeight: 'bold',
			padding: '0 4px',
			borderRadius: '2px',
			top: '-1.3em',
			letterSpacing: '0.02em'
		}
	});

	// Minimal markdown syntax highlighting mapped onto the phosphor palette
	// (headings/strong in accent green, links/emphasis in orange, etc.) so
	// note content still reads as markdown, not just plain text.
	const kvHighlightStyle = HighlightStyle.define([
		{ tag: tags.heading, color: 'var(--kv-accent)', fontWeight: 'bold' },
		{ tag: tags.strong, color: 'var(--kv-ink)', fontWeight: 'bold' },
		{ tag: tags.emphasis, color: 'var(--kv-ink)', fontStyle: 'italic' },
		{ tag: tags.link, color: 'var(--kv-orange)' },
		{ tag: tags.url, color: 'var(--kv-orange)' },
		{ tag: tags.monospace, color: 'var(--kv-accent)' },
		{ tag: tags.quote, color: 'var(--kv-dim)' },
		{ tag: tags.list, color: 'var(--kv-ink)' },
		{ tag: tags.meta, color: 'var(--kv-dim)' },
		{ tag: tags.comment, color: 'var(--kv-dim)' }
	]);

	onMount(() => {
		// Captured once at mount — the parent remounts this component per collab
		// session, so `collab` never changes under a live view.
		const collabHandles = collab;
		// In collab mode the document is seeded from the shared Y.Text (which
		// the parent only mounts us with once it's synced), so we don't set
		// `doc` — yCollab populates it. `value` is still emitted outbound for
		// word/line counts, but external `value` changes are ignored (the CRDT
		// owns the content).
		const collabExtensions: Extension[] = collabHandles
			? [
					// Bind Ctrl+Z / Ctrl+Y to the Yjs undo manager ahead of
					// CodeMirror's own history keymap so undo respects collaboration.
					Prec.high(keymap.of(yUndoManagerKeymap)),
					yCollab(collabHandles.ytext, collabHandles.awareness, {
						undoManager: collabHandles.undoManager
					}),
					remoteCaretTheme
				]
			: [];

		const state = EditorState.create({
			doc: collabHandles ? collabHandles.ytext.toString() : value,
			extensions: [
				basicSetup,
				markdown(),
				kvEditorTheme,
				syntaxHighlighting(kvHighlightStyle),
				editableCompartment.of([
					EditorView.editable.of(editable),
					EditorState.readOnly.of(!editable)
				]),
				saveKeymap(),
				...collabExtensions,
				EditorView.updateListener.of((update) => {
					if (update.docChanged && !updatingFromOutside) {
						value = update.state.doc.toString();
						onChange?.(value);
					}
				}),
				EditorView.domEventHandlers({
					blur: () => {
						onBlur?.();
					}
				}),
				...extensions
			]
		});

		view = new EditorView({
			state,
			parent: host
		});
	});

	onDestroy(() => {
		view?.destroy();
	});

	// Keep the editor in sync if `value` is changed from the outside (e.g.
	// loading a different note into the same component instance). In collab
	// mode the CRDT owns the content, so external `value` writes are ignored.
	$effect(() => {
		const newValue = value;
		if (!view || collab) return;
		const current = view.state.doc.toString();
		if (newValue !== current) {
			updatingFromOutside = true;
			view.dispatch({
				changes: { from: 0, to: current.length, insert: newValue }
			});
			updatingFromOutside = false;
		}
	});

	// React to `editable` changing after mount (e.g. permission resolved async).
	$effect(() => {
		const isEditable = editable;
		if (!view) return;
		view.dispatch({
			effects: editableCompartment.reconfigure([
				EditorView.editable.of(isEditable),
				EditorState.readOnly.of(!isEditable)
			])
		});
	});
</script>

<div class="codemirror-editor" bind:this={host}></div>

<style>
	.codemirror-editor {
		border-top: 1px solid var(--border-default);
		border-bottom: 1px solid var(--border-default);
	}

	.codemirror-editor :global(.cm-editor) {
		height: 100%;
		min-height: 60vh;
	}
</style>
