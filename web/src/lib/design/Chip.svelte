<script lang="ts">
	import type { Snippet } from 'svelte';

	let {
		color = 'accent',
		variant = 'tint',
		children
	}: {
		color?: 'accent' | 'orange' | 'danger' | 'dim';
		variant?: 'tint' | 'outline';
		children: Snippet;
	} = $props();

	const colorVar = $derived(
		{
			accent: '--kv-accent',
			orange: '--kv-orange',
			danger: '--kv-danger',
			dim: '--kv-dim'
		}[color]
	);
</script>

<span class="kv-chip {variant}" style="--chip-color: var({colorVar});">
	{@render children()}
</span>

<style>
	.kv-chip {
		display: inline-flex;
		align-items: center;
		font-family: var(--font-pixel);
		font-size: var(--type-chip);
		text-transform: uppercase;
		letter-spacing: var(--tracking-pixel);
		padding: 5px 7px;
		border-radius: var(--radius-card);
		color: var(--chip-color);
		border: 1px solid color-mix(in srgb, var(--chip-color) 35%, transparent);
	}

	.kv-chip.tint {
		background: color-mix(in srgb, var(--chip-color) 12%, transparent);
	}

	.kv-chip.outline {
		background: transparent;
		border-color: color-mix(in srgb, var(--chip-color) 40%, transparent);
	}
</style>
