<script lang="ts">
	import type { Snippet } from 'svelte';

	let {
		padded = true,
		class: className = '',
		children
	}: {
		padded?: boolean;
		class?: string;
		children: Snippet;
	} = $props();
</script>

<div class="kv-card {className}" class:padded>
	{@render children()}
</div>

<style>
	.kv-card {
		position: relative;
		background: var(--surface-card);
		border: 1px solid var(--border-default);
		border-radius: var(--radius-card);
	}

	.kv-card.padded {
		padding: 14px 16px;
	}

	.kv-card::before,
	.kv-card::after {
		content: '';
		position: absolute;
		width: 9px;
		height: 9px;
		pointer-events: none;
	}

	.kv-card::before {
		top: 6px;
		left: 6px;
		border-top: 1px solid var(--border-accent);
		border-left: 1px solid var(--border-accent);
	}

	.kv-card::after {
		bottom: 6px;
		right: 6px;
		border-bottom: 1px solid var(--border-accent);
		border-right: 1px solid var(--border-accent);
	}
</style>
