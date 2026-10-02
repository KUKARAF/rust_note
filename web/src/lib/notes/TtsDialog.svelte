<script lang="ts">
	// Text-to-speech modal, opened either from the filename speaker button
	// (whole note) or the floating selection button in CodeMirrorEditor.svelte
	// (selected text only). See docs/tts.md for the full contract.
	//
	// Backend: POST /api/tts { text, model, voice? } -> audio/wav bytes. The key
	// lives server-side only, and the response is binary, so this uses a raw
	// `rawFetch` (not the JSON `apiPost` helper) with the same auth attached.
	import { rawFetch } from '$lib/api/client';
	import { fetchAiModels } from '$lib/stores/settings';
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

	let playState = $state<'idle' | 'loading' | 'playing' | 'error'>('idle');
	let playError = $state<string | null>(null);
	let audioUrl = $state<string | null>(null);
	let audioEl: HTMLAudioElement | undefined = $state();

	function revokeAudioUrl() {
		if (audioUrl) {
			URL.revokeObjectURL(audioUrl);
			audioUrl = null;
		}
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

	async function play() {
		if (!selectedModel || playState === 'loading') return;
		playState = 'loading';
		playError = null;
		revokeAudioUrl();
		try {
			const res = await rawFetch('/api/tts', {
				method: 'POST',
				headers: { 'Content-Type': 'application/json' },
				body: JSON.stringify({ text, model: selectedModel })
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
			const blob = await res.blob();
			audioUrl = URL.createObjectURL(blob);
			playState = 'playing';
			// Set after the <audio> element re-renders with the new src; the click
			// that triggered `play()` is the user gesture that makes this allowed.
			queueMicrotask(() => {
				void audioEl?.play();
			});
		} catch (err) {
			playState = 'error';
			playError = err instanceof Error ? err.message : 'Failed to synthesize audio.';
		}
	}

	function onKeydown(event: KeyboardEvent) {
		if (event.key === 'Escape') onclose();
	}

	function onBackdropClick(event: MouseEvent) {
		if (event.target === event.currentTarget) onclose();
	}

	function close() {
		revokeAudioUrl();
		onclose();
	}

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
					{text.length} character{text.length === 1 ? '' : 's'} will be sent for synthesis.
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
						<select class="rt-select" bind:value={selectedModel}>
							{#each models as model (model)}
								<option value={model}>{model}</option>
							{/each}
						</select>
					</label>

					{#if playError}
						<p class="rt-error">{playError}</p>
					{/if}

					{#if audioUrl}
						<audio class="rt-audio" bind:this={audioEl} src={audioUrl} controls></audio>
					{/if}

					<div class="rt-actions">
						<Button
							variant="primary"
							size="sm"
							disabled={!selectedModel || playState === 'loading'}
							onclick={play}
						>
							{playState === 'loading' ? 'Synthesizing…' : 'Play'}
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

	.rt-audio {
		width: 100%;
	}

	.rt-actions {
		display: flex;
		gap: var(--space-4);
	}
</style>
