<script lang="ts">
	// Full Excalidraw editor for `.excalidraw` drawing notes, shown as a
	// fullscreen overlay above the note page.
	//
	// The official editor is a React component, so this file is a thin
	// Svelte<->React bridge: react, react-dom and @excalidraw/excalidraw are
	// all loaded via dynamic import() inside onMount and mounted imperatively
	// into a plain div (`createRoot` + `React.createElement` — the official
	// no-JSX embedding pattern). This component itself is only ever loaded
	// via dynamic import from the note page, so the whole React stack — and
	// the `index.css` imported below — rides in a lazy chunk and never
	// touches the entry bundle.
	//
	// Persistence model: edits stay inside Excalidraw's own state while
	// drawing; "Save" serializes the scene and replaces the entire collab
	// ytext in one transaction (drawings are a JSON blob, not line-mergeable
	// text — whole-document last-save-wins is the intended semantics, and
	// the note page broadcasts a soft-lock warning to peers via awareness).
	import '@excalidraw/excalidraw/index.css';

	import { onDestroy, onMount } from 'svelte';
	import type * as Y from 'yjs';
	import type { Root } from 'react-dom/client';
	import { parseExcalidrawScene } from '$lib/notes/excalidraw';
	import Button from '$lib/design/Button.svelte';

	let {
		ytext,
		onclose
	}: {
		/** Live collab text of the drawing note (bare scene JSON). */
		ytext: Y.Text;
		/** Called when the editor should go away (Close button / Escape). */
		onclose: () => void;
	} = $props();

	type ExcalidrawModule = typeof import('@excalidraw/excalidraw');

	/** Structural slice of ExcalidrawImperativeAPI — just what save() needs. */
	interface ExcalidrawApi {
		getSceneElements(): readonly unknown[];
		getAppState(): Record<string, unknown>;
		getFiles(): Record<string, unknown>;
	}

	let status = $state<'loading' | 'ready' | 'error'>('loading');
	let errorMessage = $state('');
	let dirty = $state(false);

	let hostEl: HTMLDivElement | undefined = $state();
	// Kept for save(): serializeAsJSON lives on the module namespace, which
	// only exists after the dynamic import resolves.
	let Ex: ExcalidrawModule | null = null;
	let apiRef: ExcalidrawApi | null = null;
	let root: Root | null = null;

	// Excalidraw fires onChange during its own mount and on no-op appState
	// churn (pointer position, zoom animations settling), not just on real
	// edits. A short grace window after mounting swallows that initial burst;
	// past it we accept slightly-eager dirty (a stray no-op change costs one
	// extra confirm(), never data).
	const DIRTY_GRACE_MS = 500;
	let mountedAt = 0;

	onMount(async () => {
		// Must be set BEFORE @excalidraw/excalidraw is evaluated: the package
		// reads it at module scope to locate its runtime font assets (copied
		// into static/excalidraw-assets/fonts/ by scripts/copy-excalidraw-assets.mjs).
		(window as { EXCALIDRAW_ASSET_PATH?: string }).EXCALIDRAW_ASSET_PATH = '/excalidraw-assets/';

		let React: typeof import('react');
		let ReactDOMClient: typeof import('react-dom/client');
		try {
			[React, ReactDOMClient, Ex] = await Promise.all([
				import('react'),
				import('react-dom/client'),
				import('@excalidraw/excalidraw')
			]);
		} catch (err) {
			console.error('Failed to load the Excalidraw editor', err);
			errorMessage = "The drawing editor couldn't be loaded — check your connection and retry.";
			status = 'error';
			return;
		}

		const scene = parseExcalidrawScene(ytext.toString());
		if (scene === null) {
			errorMessage =
				"This drawing couldn't be parsed — close and fix the JSON via the text editor.";
			status = 'error';
			return;
		}

		// Component may have been destroyed while the chunks loaded.
		if (hostEl === undefined) return;

		// restore() is mandatory: raw scene JSON stores `appState.collaborators`
		// as a plain object, and feeding that to the editor as initialData
		// crashes it (`collaborators.forEach is not a function`). restore()
		// normalizes the whole scene into runtime shape.
		const initialData = Ex.restore(scene as never, null, null);

		root = ReactDOMClient.createRoot(hostEl);
		mountedAt = Date.now();
		root.render(
			React.createElement(Ex.Excalidraw, {
				initialData,
				excalidrawAPI: (api: ExcalidrawApi) => {
					apiRef = api;
				},
				theme: 'dark',
				onChange: () => {
					if (Date.now() - mountedAt < DIRTY_GRACE_MS) return;
					dirty = true;
				}
			} as never)
		);
		status = 'ready';
	});

	onDestroy(() => {
		root?.unmount();
		root = null;
	});

	function save() {
		if (Ex === null || apiRef === null) return;
		// Four positional args; 'local' = include appState the way a .excalidraw
		// file on disk stores it. Output is pretty-printed, matching the
		// server-side migration format (plus our trailing newline).
		const json = Ex.serializeAsJSON(
			apiRef.getSceneElements() as never,
			apiRef.getAppState() as never,
			apiRef.getFiles() as never,
			'local'
		);
		// Whole-ytext replace in a single transaction: one CRDT update, one
		// atomic change for peers/persistence. Character-level diffing would
		// buy nothing here — concurrent drawing edits are last-save-wins by
		// design (see module comment).
		const doc = ytext.doc;
		const apply = () => {
			ytext.delete(0, ytext.length);
			ytext.insert(0, json + '\n');
		};
		if (doc) {
			doc.transact(apply);
		} else {
			apply();
		}
		dirty = false;
	}

	function requestClose() {
		if (dirty && !confirm('Discard unsaved drawing changes?')) return;
		onclose();
	}

	// Handled at the overlay root and always stopped: page-level
	// <svelte:window> handlers (the editor page's Ctrl+S hint, list
	// navigation) must never react to keystrokes meant for the drawing
	// editor. Note Escape closes the whole overlay even when pressed inside
	// the canvas — dirty state still gets its confirm() first.
	function onRootKeydown(event: KeyboardEvent) {
		event.stopPropagation();
		if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === 's') {
			event.preventDefault();
			save();
		} else if (event.key === 'Escape') {
			event.preventDefault();
			requestClose();
		}
	}
</script>

<div class="drawing-editor" onkeydown={onRootKeydown} role="presentation">
	<div class="drawing-editor-header" role="dialog" aria-modal="true" aria-label="Drawing editor">
		<span class="drawing-editor-title">&gt; drawing editor{dirty ? ' *' : ''}</span>
		<div class="drawing-editor-actions">
			{#if status !== 'error'}
				<Button variant="primary" size="sm" disabled={status !== 'ready'} onclick={save}>
					Save
				</Button>
			{/if}
			<Button variant="outline" size="sm" onclick={requestClose}>Close</Button>
		</div>
	</div>

	{#if status === 'error'}
		<p class="drawing-editor-message warn">{errorMessage}</p>
	{:else}
		{#if status === 'loading'}
			<p class="drawing-editor-message dim">Loading drawing editor…</p>
		{/if}
		<!-- React owns everything inside this div (and unmounts with the
		     component) — it must stay empty on the Svelte side, or the two
		     renderers would fight over its children. -->
		<div class="excalidraw-host" bind:this={hostEl}></div>
	{/if}
</div>

<style>
	.drawing-editor {
		position: fixed;
		inset: 0;
		display: flex;
		flex-direction: column;
		background: var(--surface-bg);
		/* Above the z-100 dialogs (Share, Track), below the z-200 command
		   palette. Safe-area padding keeps the header/canvas clear of the
		   Android status/navigation bars in the edge-to-edge app. */
		z-index: 150;
		padding-top: var(--safe-top);
		padding-bottom: var(--safe-bottom);
	}

	.drawing-editor-header {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: var(--space-5);
		padding: var(--space-4) calc(var(--space-5) + var(--safe-right)) var(--space-4)
			calc(var(--space-5) + var(--safe-left));
		border-bottom: 1px solid var(--border-default);
		background: var(--surface-card);
	}

	.drawing-editor-title {
		font-family: var(--font-pixel);
		font-size: var(--type-label);
		text-transform: uppercase;
		letter-spacing: var(--tracking-pixel);
		color: var(--kv-accent);
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}

	.drawing-editor-actions {
		display: flex;
		align-items: center;
		gap: var(--space-4);
		flex: 0 0 auto;
	}

	.drawing-editor-message {
		margin: var(--space-8) var(--space-6);
		font-family: var(--font-term);
		font-size: var(--type-body);
		text-align: center;
	}

	.drawing-editor-message.dim {
		color: var(--kv-dim);
	}

	.drawing-editor-message.warn {
		color: var(--kv-orange);
	}

	.excalidraw-host {
		flex: 1;
		/* Flex children default to min-height:auto — without this the canvas
		   could push the header off-screen instead of shrinking. */
		min-height: 0;
	}
</style>
