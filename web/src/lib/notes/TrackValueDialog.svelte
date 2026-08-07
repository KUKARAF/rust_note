<script lang="ts">
	// Quick metric-tracking dialog for daily notes: appends a structured
	// frontmatter entry (see $lib/notes/metrics.ts for the format) to the
	// live collab doc — the edit rides through the CRDT exactly like typing,
	// so it works offline and syncs/mirrors automatically.
	import type * as Y from 'yjs';
	import { readMetrics, appendMetricEntry, normalizeMetricName } from '$lib/notes/metrics';
	import Button from '$lib/design/Button.svelte';
	import Card from '$lib/design/Card.svelte';
	import Input from '$lib/design/Input.svelte';

	let {
		ytext,
		onclose
	}: {
		ytext: Y.Text;
		onclose: () => void;
	} = $props();

	/** Vault keys worth suggesting even when today's note doesn't have them yet. */
	const KNOWN_METRICS = ['distractions', 'pomodoro', 'calories_eaten', 'weight', 'sleep_hours'];

	function currentTime(): string {
		const now = new Date();
		return `${String(now.getHours()).padStart(2, '0')}:${String(now.getMinutes()).padStart(2, '0')}`;
	}

	let metricName = $state('');
	let valueText = $state('');
	// Prefilled with "now"; clearing it records an untimed entry.
	let timeText = $state(currentTime());
	let error = $state<string | null>(null);
	let metricInput: HTMLInputElement | undefined = $state();

	const suggestions = $derived.by(() => {
		const fromDoc = [...readMetrics(ytext.toString()).keys()];
		const all = [...new Set([...fromDoc, ...KNOWN_METRICS])];
		const q = normalizeMetricName(metricName);
		return (q === '' ? all : all.filter((k) => k.includes(q))).slice(0, 6);
	});

	$effect(() => {
		metricInput?.focus();
	});

	function save() {
		error = null;
		const value = Number(valueText.trim());
		if (normalizeMetricName(metricName) === '') {
			error = 'Metric name is required.';
			return;
		}
		if (valueText.trim() === '' || !Number.isFinite(value)) {
			error = 'Value must be a number.';
			return;
		}
		const time = timeText.trim();
		if (time !== '' && !/^\d{2}:\d{2}$/.test(time)) {
			error = 'Time must be HH:MM (or empty for no time).';
			return;
		}

		const docText = ytext.toString();
		const next = appendMetricEntry(docText, metricName, value, time === '' ? undefined : time);
		if (next === null) {
			error = "This note's frontmatter is malformed — fix it in the editor first.";
			return;
		}
		// Replace the changed prefix only (frontmatter block), not the whole
		// doc: a full-doc replace would churn the CRDT and peers' cursors.
		// Both texts share the body suffix, so diff to the common suffix start.
		let prefixEnd = 0;
		const minLen = Math.min(docText.length, next.length);
		while (prefixEnd < minLen && docText[prefixEnd] === next[prefixEnd]) prefixEnd += 1;
		let suffixLen = 0;
		while (
			suffixLen < minLen - prefixEnd &&
			docText[docText.length - 1 - suffixLen] === next[next.length - 1 - suffixLen]
		) {
			suffixLen += 1;
		}
		const doc = ytext.doc;
		const applyEdit = () => {
			ytext.delete(prefixEnd, docText.length - prefixEnd - suffixLen);
			ytext.insert(prefixEnd, next.slice(prefixEnd, next.length - suffixLen));
		};
		if (doc) {
			doc.transact(applyEdit);
		} else {
			applyEdit();
		}
		onclose();
	}

	function onKeydown(event: KeyboardEvent) {
		event.stopPropagation();
		if (event.key === 'Escape') {
			event.preventDefault();
			onclose();
		} else if (event.key === 'Enter') {
			event.preventDefault();
			save();
		}
	}

	function onBackdropClick(event: MouseEvent) {
		if (event.target === event.currentTarget) onclose();
	}
</script>

<div class="track-backdrop" onclick={onBackdropClick} onkeydown={onKeydown} role="presentation">
	<div class="track-wrap" role="dialog" aria-modal="true" aria-label="Track a value">
		<Card>
			<div class="track">
				<span class="track-title">&gt; track value</span>

				<Input
					label="Metric"
					placeholder="calories_eaten"
					bind:value={metricName}
					bind:el={metricInput}
				/>
				{#if suggestions.length > 0}
					<div class="track-suggestions">
						{#each suggestions as key (key)}
							<button type="button" class="track-suggestion" onclick={() => (metricName = key)}>
								{key}
							</button>
						{/each}
					</div>
				{/if}

				<Input label="Value" type="text" placeholder="300" bind:value={valueText} />
				<Input label="Time (optional)" type="text" placeholder="HH:MM" bind:value={timeText} />

				{#if error}
					<p class="track-error">{error}</p>
				{/if}

				<div class="track-actions">
					<Button variant="primary" size="sm" onclick={save}>Add entry</Button>
					<Button variant="outline" size="sm" onclick={onclose}>Cancel</Button>
				</div>
			</div>
		</Card>
	</div>
</div>

<style>
	.track-backdrop {
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

	.track-wrap {
		width: 100%;
		max-width: 24rem;
	}

	.track {
		display: flex;
		flex-direction: column;
		gap: var(--space-5);
		padding: var(--space-5);
	}

	.track-title {
		font-family: var(--font-pixel);
		font-size: var(--type-label);
		text-transform: uppercase;
		letter-spacing: var(--tracking-pixel);
		color: var(--kv-accent);
	}

	.track-suggestions {
		display: flex;
		flex-wrap: wrap;
		gap: var(--space-2);
	}

	.track-suggestion {
		background: transparent;
		border: 1px solid var(--border-default);
		border-radius: var(--radius-control);
		color: var(--kv-dim);
		font-family: var(--font-term);
		font-size: var(--type-meta);
		padding: 3px 7px;
		cursor: pointer;
	}

	.track-suggestion:hover {
		border-color: var(--kv-accent);
		color: var(--kv-ink);
	}

	.track-error {
		margin: 0;
		font-family: var(--font-term);
		color: var(--kv-danger);
	}

	.track-actions {
		display: flex;
		gap: var(--space-4);
	}
</style>
