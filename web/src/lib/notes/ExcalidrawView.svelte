<script lang="ts">
	// Read-only Excalidraw renderer: parses the note's drawing block and
	// renders it to a self-contained SVG via the official @excalidraw/utils
	// exportToSvg (fonts are inlined as data URIs — no CDN, CSP-clean).
	//
	// The utils package is browser-only and heavy, so it's loaded via dynamic
	// import the first time a drawing renders (same lazy pattern as the
	// android-fs plugin in noteMirror.ts) and never touches the entry bundle.
	// NOTE: the package is pinned EXACTLY (=0.1.3-test32) in package.json —
	// upstream only publishes test-tagged builds of @excalidraw/utils; the
	// pin is validated by parsing/rendering the user's real vault. If it ever
	// breaks, the fallback is importing exportToSvg from the main
	// @excalidraw/excalidraw package (needs react/react-dom installed).
	import { parseExcalidrawScene } from '$lib/notes/excalidraw';

	let {
		content,
		onunrenderable
	}: {
		/** Current full note text (markdown wrapper or bare scene JSON). */
		content: string;
		/** Called when the content has no parseable/renderable drawing. */
		onunrenderable?: () => void;
	} = $props();

	let container: HTMLDivElement | undefined = $state();
	let status = $state<'loading' | 'ok' | 'failed'>('loading');

	/** Re-render at most ~once a second while collab edits stream in. */
	const RENDER_DEBOUNCE_MS = 800;
	let timer: ReturnType<typeof setTimeout> | undefined;
	let renderSeq = 0;

	async function render(text: string) {
		const seq = ++renderSeq;
		const scene = parseExcalidrawScene(text);
		if (scene === null) {
			status = 'failed';
			onunrenderable?.();
			return;
		}
		try {
			const utils = await import('@excalidraw/utils');
			const svg = await utils.exportToSvg({
				elements: scene.elements as never[],
				appState: { ...scene.appState, exportBackground: false } as never,
				files: (scene.files ?? {}) as never,
				exportPadding: 16
			});
			// A newer render may have started while the chunk/fonts loaded.
			if (seq !== renderSeq || container === undefined) return;
			svg.removeAttribute('width');
			svg.removeAttribute('height');
			// The bound div is deliberately empty on the Svelte side; swapping the
			// exported SVG in manually is the whole rendering mechanism here.
			// eslint-disable-next-line svelte/no-dom-manipulating
			container.replaceChildren(svg);
			status = 'ok';
		} catch (err) {
			console.error('Excalidraw render failed', err);
			if (seq === renderSeq) {
				status = 'failed';
				onunrenderable?.();
			}
		}
	}

	$effect(() => {
		const text = content;
		clearTimeout(timer);
		// First render immediately; subsequent content changes debounce.
		if (status === 'loading') {
			void render(text);
		} else {
			timer = setTimeout(() => void render(text), RENDER_DEBOUNCE_MS);
		}
		return () => clearTimeout(timer);
	});
</script>

<div class="drawing-wrap">
	{#if status === 'loading'}
		<p class="drawing-status">Rendering drawing…</p>
	{:else if status === 'failed'}
		<p class="drawing-status warn">This drawing couldn't be rendered — showing raw text instead.</p>
	{/if}
	<div class="drawing-svg" bind:this={container}></div>
</div>

<style>
	.drawing-wrap {
		flex: 1;
		overflow: auto;
		background: var(--surface-card);
		border: 1px solid var(--border-default);
		border-radius: var(--radius-card);
		padding: var(--space-5);
	}

	.drawing-status {
		margin: 0 0 var(--space-4);
		font-family: var(--font-term);
		color: var(--kv-dim);
	}

	.drawing-status.warn {
		color: var(--kv-orange);
	}

	.drawing-svg :global(svg) {
		max-width: 100%;
		height: auto;
		display: block;
		margin: 0 auto;
	}
</style>
