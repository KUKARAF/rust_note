<script lang="ts">
	import type { Snippet } from 'svelte';

	let {
		value = $bindable(''),
		type = 'text',
		placeholder = '',
		label,
		id,
		el = $bindable(undefined),
		prefix,
		suffix,
		readonly = false,
		onkeydown,
		onclick
	}: {
		value?: string;
		type?: string;
		placeholder?: string;
		label?: string;
		id?: string;
		el?: HTMLInputElement;
		prefix?: Snippet;
		suffix?: Snippet;
		readonly?: boolean;
		onkeydown?: (event: KeyboardEvent) => void;
		onclick?: (event: MouseEvent) => void;
	} = $props();
</script>

<div class="kv-input-wrap">
	{#if label}
		<label class="kv-input-label" for={id}>{label}</label>
	{/if}
	<div class="kv-input-field">
		{#if prefix}
			<span class="kv-input-prefix">{@render prefix()}</span>
		{/if}
		<input
			class="kv-input"
			{id}
			{type}
			{placeholder}
			{readonly}
			bind:value
			bind:this={el}
			{onkeydown}
			{onclick}
		/>
		{#if suffix}
			<span class="kv-input-suffix">{@render suffix()}</span>
		{/if}
	</div>
</div>

<style>
	.kv-input-wrap {
		display: flex;
		flex-direction: column;
		gap: var(--space-2);
	}

	.kv-input-label {
		font-family: var(--font-pixel);
		font-size: var(--type-label);
		text-transform: uppercase;
		letter-spacing: var(--tracking-pixel);
		color: var(--kv-dim);
	}

	.kv-input-field {
		display: flex;
		align-items: center;
		gap: var(--space-2);
		background: var(--surface-input);
		border: 1px solid var(--border-default);
		border-radius: var(--radius-control);
		padding: 0 10px;
		transition: border-color 120ms linear;
	}

	.kv-input-field:focus-within {
		border-color: var(--kv-accent);
	}

	.kv-input-prefix {
		color: var(--kv-accent);
		font-family: var(--font-term);
		font-size: var(--type-body);
		line-height: 1;
	}

	.kv-input-suffix {
		font-family: var(--font-pixel);
		font-size: var(--type-chip);
		color: var(--kv-dim);
		text-transform: uppercase;
		border: 1px solid var(--border-default);
		border-radius: var(--radius-control);
		padding: 3px 5px;
		white-space: nowrap;
	}

	.kv-input {
		flex: 1;
		min-width: 0;
		background: transparent;
		border: none;
		outline: none;
		padding: 9px 0;
		font-family: var(--font-term);
		font-size: var(--type-body);
		color: var(--kv-ink);
		caret-color: var(--kv-accent);
	}

	.kv-input::placeholder {
		color: var(--kv-placeholder);
	}
</style>
