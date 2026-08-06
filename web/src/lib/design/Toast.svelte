<script lang="ts">
	import BlinkingCursor from './BlinkingCursor.svelte';

	let {
		message,
		color = 'var(--kv-accent)',
		showCursor = true
	}: {
		message: string;
		color?: string;
		showCursor?: boolean;
	} = $props();
</script>

<div class="kv-toast" style="--toast-color: {color};">
	<span class="kv-toast-prefix">&gt;</span>
	<span class="kv-toast-message">{message}</span>
	{#if showCursor}
		<BlinkingCursor {color} />
	{/if}
</div>

<style>
	.kv-toast {
		display: inline-flex;
		align-items: center;
		gap: var(--space-2);
		background: #0c160c;
		border: 1px solid var(--toast-color);
		border-radius: var(--radius-control);
		padding: 8px 12px;
		font-family: var(--font-term);
		font-size: var(--type-meta);
		color: var(--toast-color);
	}
</style>
