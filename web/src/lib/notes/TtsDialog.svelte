<script lang="ts">
	// Text-to-speech modal, opened either from the filename speaker button
	// (whole note) or the floating selection button in CodeMirrorEditor.svelte
	// (selected text only). See docs/tts.md for the full contract.
	//
	// Backend: POST /api/tts { text, model, voice? } -> audio/wav bytes, ONE
	// small chunk at a time (see ttsChunk.ts). The key lives server-side only,
	// and the response is binary, so this uses a raw `rawFetch` (not the JSON
	// `apiPost` helper) with the same auth attached.
	//
	// Playback is chunked and progressively buffered rather than one big
	// synthesis request: the input text is split into small ordered chunks,
	// each is synthesized with its own POST, and a rolling window of chunks
	// ahead of the one currently playing is kept pre-fetched so the NEXT
	// chunk's audio is ready before the current one finishes. Chunks play
	// back-to-back through a single <audio> element; small gaps at sentence
	// boundaries are expected and fine (no attempt at gapless/Web Audio
	// stitching). This makes playback start fast (first chunk is small) and
	// keeps one bad/slow chunk from blocking or failing the whole note.
	import { onDestroy } from 'svelte';
	import { rawFetch } from '$lib/api/client';
	import { fetchAiModels } from '$lib/stores/settings';
	import { chunkText, MAX_TOTAL_WORDS, MAX_WORDS_PER_CHUNK } from './ttsChunk';
	import Card from '$lib/design/Card.svelte';
	import Button from '$lib/design/Button.svelte';

	let {
		text,
		onclose
	}: {
		text: string;
		onclose: () => void;
	} = $props();

	type LoadState = 'loading' | 'ok' | 'no-key' | 'error';

	let loadState = $state<LoadState>('loading');
	let loadError = $state<string | null>(null);
	let models = $state<string[]>([]);
	let selectedModel = $state('');

	// --- Chunked playback state -------------------------------------------

	/** Default for how many chunks beyond the currently-playing one to keep pre-fetched. */
	const DEFAULT_PREFETCH_AHEAD = 2;

	// --- Advanced (user-tunable) settings ----------------------------------
	// Live-editable from the "Advanced" disclosure, but only taken into account
	// the next time playback (re)starts from a stopped/idle/done/error state —
	// see `play()`. This keeps mid-playback state simple: no re-chunking or
	// resizing the prefetch window out from under an in-flight buffer.

	let maxWordsPerChunk = $state(MAX_WORDS_PER_CHUNK);
	let prefetchAhead = $state(DEFAULT_PREFETCH_AHEAD);

	const MIN_WORDS_PER_CHUNK = 5;
	const MAX_WORDS_PER_CHUNK_BOUND = 500;
	const MIN_PREFETCH_AHEAD = 1;
	const MAX_PREFETCH_AHEAD = 10;

	function clampInt(n: number, min: number, max: number): number {
		if (!Number.isFinite(n)) return min;
		return Math.min(max, Math.max(min, Math.round(n)));
	}

	function onMaxWordsChange() {
		maxWordsPerChunk = clampInt(maxWordsPerChunk, MIN_WORDS_PER_CHUNK, MAX_WORDS_PER_CHUNK_BOUND);
	}

	function onPrefetchAheadChange() {
		prefetchAhead = clampInt(prefetchAhead, MIN_PREFETCH_AHEAD, MAX_PREFETCH_AHEAD);
	}

	/** Cheap preview of how many chunks the current cap would produce, for the Advanced panel. */
	let previewChunkCount = $derived(chunkText(text, { maxWordsPerChunk }).chunks.length);

	type ChunkStatus = 'idle' | 'loading' | 'ready' | 'error' | 'done';

	interface ChunkSlot {
		status: ChunkStatus;
		url: string | null;
		error: string | null;
	}

	type PlayState = 'idle' | 'buffering' | 'playing' | 'paused' | 'done' | 'error';

	let chunks = $state<string[]>([]);
	let slots = $state<ChunkSlot[]>([]);
	let currentIndex = $state(0);
	let currentAudioUrl = $state<string | null>(null);
	let playState = $state<PlayState>('idle');
	let playError = $state<string | null>(null);
	let truncatedNotice = $state<string | null>(null);
	let audioEl: HTMLAudioElement | undefined = $state();

	// Plain (non-reactive) bookkeeping: one AbortController per in-flight fetch,
	// keyed by chunk index, so Stop/close can abort everything outstanding.
	// Intentionally a vanilla Map, not SvelteMap — nothing in the template reads
	// this, so it doesn't need to participate in Svelte's reactivity.
	// eslint-disable-next-line svelte/prefer-svelte-reactivity
	const controllers = new Map<number, AbortController>();

	/** Abort every in-flight fetch and revoke every outstanding object URL. */
	function abortAndRevokeAll() {
		for (const controller of controllers.values()) {
			controller.abort();
		}
		controllers.clear();
		for (const slot of slots) {
			if (slot.url) {
				URL.revokeObjectURL(slot.url);
				slot.url = null;
			}
		}
		currentAudioUrl = null;
		audioEl?.pause();
	}

	async function fetchChunk(index: number) {
		const slot = slots[index];
		if (!slot || slot.status === 'loading' || slot.status === 'ready') return;
		slot.status = 'loading';
		slot.error = null;

		const controller = new AbortController();
		controllers.set(index, controller);
		try {
			const res = await rawFetch('/api/tts', {
				method: 'POST',
				headers: { 'Content-Type': 'application/json' },
				body: JSON.stringify({ text: chunks[index], model: selectedModel }),
				signal: controller.signal
			});
			if (!res.ok) {
				let message = `Request failed with status ${res.status}`;
				try {
					const body = await res.json();
					if (body && typeof body === 'object' && 'message' in body) {
						message = String((body as Record<string, unknown>).message);
					}
				} catch {
					// non-JSON error body — keep the generic message
				}
				throw new Error(message);
			}
			const contentType = res.headers.get('content-type') ?? '';
			if (!contentType.startsWith('audio/')) {
				throw new Error(`Unexpected response type: ${contentType || 'unknown'}`);
			}
			const blob = await res.blob();
			slot.url = URL.createObjectURL(blob);
			slot.status = 'ready';
			// A slot just freed up (conceptually) — keep the prefetch window full.
			ensurePrefetch();
		} catch (err) {
			if (err instanceof DOMException && err.name === 'AbortError') {
				// Stopped/closed mid-flight — not a user-facing error.
				return;
			}
			slot.status = 'error';
			slot.error = err instanceof Error ? err.message : 'Failed to synthesize audio.';
		} finally {
			controllers.delete(index);
		}
	}

	/** Kick off fetches for every idle chunk in [currentIndex, currentIndex + prefetchAhead]. */
	function ensurePrefetch() {
		const end = Math.min(currentIndex + prefetchAhead, chunks.length - 1);
		for (let i = currentIndex; i <= end; i++) {
			const slot = slots[i];
			if (slot && slot.status === 'idle') void fetchChunk(i);
		}
	}

	function startCurrentChunk() {
		const slot = slots[currentIndex];
		if (!slot?.url) return;
		currentAudioUrl = slot.url;
		playState = 'playing';
		// The click that triggered the first `play()` is the user gesture that
		// makes autoplay-on-src-change allowed; subsequent chunks are chained
		// from the `ended` event of a already-playing element, which is fine.
		queueMicrotask(() => {
			void audioEl?.play();
		});
	}

	// Whenever we're waiting on the chunk at `currentIndex`, start it as soon
	// as it's ready, or surface its error.
	$effect(() => {
		if (playState !== 'buffering') return;
		const slot = slots[currentIndex];
		if (!slot) return;
		if (slot.status === 'ready') {
			startCurrentChunk();
		} else if (slot.status === 'error') {
			playState = 'error';
			playError = `Chunk ${currentIndex + 1} of ${chunks.length} failed: ${
				slot.error ?? 'unknown error'
			}`;
			abortAndRevokeAll();
		}
	});

	function handleEnded() {
		const finished = slots[currentIndex];
		if (finished?.url) {
			URL.revokeObjectURL(finished.url);
			finished.url = null;
			finished.status = 'done';
		}
		currentAudioUrl = null;
		currentIndex += 1;
		if (currentIndex >= chunks.length) {
			playState = 'done';
			return;
		}
		playState = 'buffering';
		ensurePrefetch();
	}

	async function loadModels() {
		loadState = 'loading';
		loadError = null;
		try {
			const res = await fetchAiModels();
			if (res.error) {
				loadState = 'no-key';
				loadError = res.error;
				return;
			}
			const ttsModels = (res.models ?? []).filter((m) => m.endsWith('-tts'));
			models = ttsModels;
			if (ttsModels.length === 0) {
				loadState = 'no-key';
				loadError = 'No text-to-speech models are available on the configured AI endpoint.';
				return;
			}
			selectedModel = ttsModels[0];
			loadState = 'ok';
		} catch (err) {
			loadState = 'error';
			loadError = err instanceof Error ? err.message : 'Failed to load TTS models.';
		}
	}

	function play() {
		if (!selectedModel) return;

		if (playState === 'paused') {
			playState = 'playing';
			void audioEl?.play();
			return;
		}
		if (playState === 'playing' || playState === 'buffering') return;

		// Fresh start: first Play, a Replay after completion, or a retry after
		// an error. Re-chunk (text doesn't change while the dialog is open, but
		// this keeps the fresh-start path self-contained) and reset everything.
		abortAndRevokeAll();
		const result = chunkText(text, { maxWordsPerChunk });
		if (result.chunks.length === 0) {
			playState = 'error';
			playError = 'There is no text to read.';
			return;
		}
		chunks = result.chunks;
		slots = chunks.map(() => ({ status: 'idle', url: null, error: null }));
		currentIndex = 0;
		truncatedNotice = result.truncated
			? `This note is long (${result.totalWords} words) — reading only the first ` +
				`${chunks.length} chunk(s), up to the ${MAX_TOTAL_WORDS}-word safety limit.`
			: null;

		playState = 'buffering';
		playError = null;
		ensurePrefetch();
	}

	function pause() {
		audioEl?.pause();
		if (playState === 'playing') playState = 'paused';
	}

	function stop() {
		abortAndRevokeAll();
		chunks = [];
		slots = [];
		currentIndex = 0;
		playState = 'idle';
		playError = null;
		truncatedNotice = null;
	}

	function onKeydown(event: KeyboardEvent) {
		if (event.key === 'Escape') onclose();
	}

	function onBackdropClick(event: MouseEvent) {
		if (event.target === event.currentTarget) onclose();
	}

	function close() {
		abortAndRevokeAll();
		onclose();
	}

	onDestroy(() => {
		abortAndRevokeAll();
	});

	loadModels();
</script>

<svelte:window onkeydown={onKeydown} />

<div class="rt-backdrop" onclick={onBackdropClick} role="presentation">
	<div class="rt-wrap" role="dialog" aria-modal="true" aria-label="Read aloud">
		<Card>
			<div class="rt">
				<div class="rt-header">
					<span class="rt-eyebrow">&gt; read aloud</span>
					<button class="rt-close" onclick={close} aria-label="Close">✕</button>
				</div>

				<p class="rt-caption">
					{text.length} character{text.length === 1 ? '' : 's'} will be sent for synthesis, in small chunks.
				</p>

				{#if loadState === 'loading'}
					<p class="rt-hint">loading models…</p>
				{:else if loadState === 'no-key' || loadState === 'error'}
					<p class="rt-error">
						{loadError ?? 'Set an API key in Settings first.'}
					</p>
					<Button variant="outline" size="sm" onclick={loadModels}>Retry</Button>
				{:else}
					<label class="rt-field">
						<span class="rt-field-label">Voice (model)</span>
						<select class="rt-select" bind:value={selectedModel} disabled={playState !== 'idle'}>
							{#each models as model (model)}
								<option value={model}>{model}</option>
							{/each}
						</select>
					</label>

					<details class="rt-advanced">
						<summary class="rt-advanced-summary">Advanced</summary>
						<div class="rt-advanced-body">
							<label class="rt-field">
								<span class="rt-field-label">Max words per chunk</span>
								<input
									class="rt-number"
									type="number"
									min={MIN_WORDS_PER_CHUNK}
									max={MAX_WORDS_PER_CHUNK_BOUND}
									step="1"
									bind:value={maxWordsPerChunk}
									onchange={onMaxWordsChange}
									disabled={playState !== 'idle'}
								/>
							</label>
							<label class="rt-field">
								<span class="rt-field-label">Buffer size (chunks ahead)</span>
								<input
									class="rt-number"
									type="number"
									min={MIN_PREFETCH_AHEAD}
									max={MAX_PREFETCH_AHEAD}
									step="1"
									bind:value={prefetchAhead}
									onchange={onPrefetchAheadChange}
									disabled={playState !== 'idle'}
								/>
							</label>
							<p class="rt-hint">
								{previewChunkCount} chunk{previewChunkCount === 1 ? '' : 's'} at this cap · takes effect
								on next Play
							</p>
						</div>
					</details>

					{#if truncatedNotice}
						<p class="rt-hint">{truncatedNotice}</p>
					{/if}

					{#if playError}
						<p class="rt-error">{playError}</p>
					{/if}

					{#if chunks.length > 0}
						<p class="rt-hint">
							chunk {Math.min(currentIndex + 1, chunks.length)} / {chunks.length}
							{#if playState === 'buffering'}(buffering…){/if}
						</p>
					{/if}

					<audio class="rt-audio" bind:this={audioEl} src={currentAudioUrl} onended={handleEnded}
					></audio>

					<div class="rt-actions">
						{#if playState === 'playing'}
							<Button variant="primary" size="sm" onclick={pause}>Pause</Button>
						{:else if playState === 'paused'}
							<Button variant="primary" size="sm" onclick={play}>Resume</Button>
						{:else if playState === 'buffering'}
							<Button variant="primary" size="sm" disabled>Buffering…</Button>
						{:else if playState === 'done'}
							<Button variant="primary" size="sm" onclick={play}>Replay</Button>
						{:else}
							<Button variant="primary" size="sm" disabled={!selectedModel} onclick={play}>
								Play
							</Button>
						{/if}
						<Button variant="outline" size="sm" disabled={playState === 'idle'} onclick={stop}>
							Stop
						</Button>
						<Button variant="outline" size="sm" onclick={close}>Close</Button>
					</div>
				{/if}
			</div>
		</Card>
	</div>
</div>

<style>
	.rt-backdrop {
		position: fixed;
		inset: 0;
		background: rgba(3, 6, 3, 0.72);
		display: flex;
		align-items: flex-start;
		justify-content: center;
		padding: calc(var(--space-9) + var(--safe-top)) calc(var(--space-6) + var(--safe-right))
			calc(var(--space-9) + var(--safe-bottom)) calc(var(--space-6) + var(--safe-left));
		overflow-y: auto;
		z-index: 100;
	}

	.rt-wrap {
		width: 100%;
		max-width: 28rem;
	}

	.rt {
		display: flex;
		flex-direction: column;
		gap: var(--space-5);
	}

	.rt-header {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: var(--space-4);
	}

	.rt-eyebrow {
		font-family: var(--font-pixel);
		font-size: var(--type-label);
		letter-spacing: var(--tracking-pixel);
		text-transform: uppercase;
		color: var(--kv-accent);
	}

	.rt-close {
		flex: 0 0 auto;
		background: transparent;
		border: 1px solid var(--border-default);
		border-radius: var(--radius-control);
		color: var(--kv-dim);
		font-family: var(--font-pixel);
		font-size: var(--type-label);
		cursor: pointer;
		padding: 6px 8px;
	}

	.rt-close:hover {
		color: var(--kv-ink);
		border-color: var(--border-accent);
	}

	.rt-caption,
	.rt-hint {
		margin: 0;
		font-family: var(--font-term);
		font-size: var(--type-meta);
		color: var(--kv-dim);
	}

	.rt-error {
		margin: 0;
		font-family: var(--font-term);
		font-size: var(--type-meta);
		color: var(--kv-danger);
	}

	.rt-field {
		display: flex;
		flex-direction: column;
		gap: var(--space-2);
	}

	.rt-field-label {
		font-family: var(--font-term);
		font-size: var(--type-meta);
		color: var(--kv-dim);
	}

	.rt-select {
		background: var(--surface-input);
		border: 1px solid var(--border-default);
		border-radius: var(--radius-control);
		padding: 7px 9px;
		font-family: var(--font-term);
		font-size: var(--type-body);
		color: var(--kv-ink);
	}

	.rt-select:focus {
		outline: none;
		border-color: var(--kv-accent);
	}

	.rt-select:disabled {
		opacity: 0.6;
	}

	.rt-advanced {
		border: 1px solid var(--border-default);
		border-radius: var(--radius-control);
		padding: var(--space-3);
	}

	.rt-advanced-summary {
		cursor: pointer;
		font-family: var(--font-pixel);
		font-size: var(--type-label);
		letter-spacing: var(--tracking-pixel);
		text-transform: uppercase;
		color: var(--kv-dim);
		user-select: none;
	}

	.rt-advanced-summary:hover {
		color: var(--kv-ink);
	}

	.rt-advanced-body {
		display: flex;
		flex-direction: column;
		gap: var(--space-3);
		margin-top: var(--space-3);
	}

	.rt-number {
		width: 6rem;
		background: var(--surface-input);
		border: 1px solid var(--border-default);
		border-radius: var(--radius-control);
		padding: 7px 9px;
		font-family: var(--font-term);
		font-size: var(--type-body);
		color: var(--kv-ink);
	}

	.rt-number:focus {
		outline: none;
		border-color: var(--kv-accent);
	}

	.rt-number:disabled {
		opacity: 0.6;
	}

	.rt-audio {
		display: none;
	}

	.rt-actions {
		display: flex;
		gap: var(--space-4);
	}
</style>
