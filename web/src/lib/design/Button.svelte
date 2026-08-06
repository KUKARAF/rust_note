<script lang="ts">
	import type { Snippet } from 'svelte';

	let {
		variant = 'primary',
		size = 'md',
		type = 'button',
		disabled = false,
		onclick,
		children
	}: {
		variant?: 'primary' | 'outline' | 'danger';
		size?: 'sm' | 'md' | 'lg';
		type?: 'button' | 'submit' | 'reset';
		disabled?: boolean;
		onclick?: (event: MouseEvent) => void;
		children: Snippet;
	} = $props();
</script>

<button
	class="kv-button {variant} {size}"
	{type}
	{disabled}
	onclick={(e) => onclick?.(e)}
>
	{@render children()}
</button>

<style>
	.kv-button {
		font-family: var(--font-pixel);
		text-transform: uppercase;
		letter-spacing: var(--tracking-pixel);
		border-radius: var(--radius-control);
		cursor: pointer;
		transition: opacity 120ms linear;
		border: 1px solid transparent;
		white-space: nowrap;
	}

	.kv-button:hover:not(:disabled) {
		opacity: 0.82;
	}

	.kv-button:disabled {
		cursor: not-allowed;
		opacity: 0.85;
	}

	/* Sizes */
	.sm {
		padding: 8px 12px;
		font-size: 8px;
	}
	.md {
		padding: 11px 16px;
		font-size: 9px;
	}
	.lg {
		padding: 14px 22px;
		font-size: 11px;
	}

	/* Variants */
	.primary {
		background: var(--kv-accent);
		border-color: var(--kv-accent);
		color: var(--kv-accent-ink);
	}
	.primary:disabled {
		background: var(--kv-faint);
		border-color: var(--kv-faint);
		color: var(--kv-dim);
	}

	.outline {
		background: transparent;
		border-color: var(--border-accent);
		color: var(--kv-accent);
	}
	.outline:disabled {
		border-color: var(--kv-faint);
		color: var(--kv-dim);
	}

	.danger {
		background: rgba(255, 107, 107, 0.12);
		border-color: var(--kv-danger);
		color: var(--kv-danger);
	}
	.danger:disabled {
		background: transparent;
		border-color: var(--kv-faint);
		color: var(--kv-dim);
	}
</style>
