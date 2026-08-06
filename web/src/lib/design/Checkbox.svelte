<script lang="ts">
	let {
		checked = $bindable(false),
		label,
		id
	}: {
		checked?: boolean;
		label?: string;
		id?: string;
	} = $props();
</script>

<label class="kv-checkbox-row" for={id}>
	<span class="kv-checkbox" class:checked>
		<input type="checkbox" {id} bind:checked class="kv-checkbox-input" />
		{#if checked}
			<span class="kv-checkbox-fill"></span>
		{/if}
	</span>
	{#if label}
		<span class="kv-checkbox-label">{label}</span>
	{/if}
</label>

<style>
	.kv-checkbox-row {
		display: inline-flex;
		align-items: center;
		gap: var(--space-3);
		cursor: pointer;
	}

	.kv-checkbox {
		position: relative;
		width: 20px;
		height: 20px;
		flex: 0 0 auto;
		background: var(--surface-input);
		border: 1px solid var(--border-default);
		border-radius: var(--radius-control);
		display: flex;
		align-items: center;
		justify-content: center;
	}

	.kv-checkbox.checked {
		border-color: var(--kv-accent);
	}

	.kv-checkbox-input {
		position: absolute;
		inset: 0;
		opacity: 0;
		margin: 0;
		cursor: pointer;
	}

	.kv-checkbox-fill {
		width: 12px;
		height: 12px;
		background: var(--kv-accent);
		pointer-events: none;
	}

	.kv-checkbox-label {
		font-family: var(--font-term);
		font-size: var(--type-body);
		color: var(--kv-ink);
	}
</style>
