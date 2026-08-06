<script lang="ts">
	import type { Snippet } from 'svelte';

	let {
		color = 'var(--kv-orange)',
		children
	}: {
		color?: string;
		children: Snippet;
	} = $props();
</script>

<div class="kv-section-title" style="--title-color: {color};">
	<span class="kv-section-title-prefix">&gt;</span>
	{@render children()}
</div>

<style>
	.kv-section-title {
		font-family: var(--font-pixel);
		font-size: var(--type-heading);
		text-transform: uppercase;
		letter-spacing: var(--tracking-pixel);
		color: var(--title-color);
		display: flex;
		align-items: center;
		gap: var(--space-2);
	}

	.kv-section-title-prefix {
		color: var(--title-color);
	}
</style>
