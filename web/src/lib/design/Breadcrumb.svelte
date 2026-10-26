<script lang="ts">
	let {
		crumbs,
		onnavigate
	}: {
		crumbs: Array<{ label: string; path: string }>;
		onnavigate: (path: string) => void;
	} = $props();
</script>

<nav class="breadcrumb" aria-label="Folder navigation">
	{#each crumbs as crumb, i (crumb.path)}
		{#if i > 0}
			<span class="sep" aria-hidden="true">›</span>
		{/if}
		{#if i === crumbs.length - 1}
			<span class="current">{crumb.label}</span>
		{:else}
			<button type="button" class="crumb" onclick={() => onnavigate(crumb.path)}>
				{crumb.label}
			</button>
		{/if}
	{/each}
</nav>

<style>
	.breadcrumb {
		display: flex;
		align-items: center;
		gap: 2px;
		font-family: var(--font-term);
		font-size: var(--type-meta);
		flex-wrap: wrap;
		margin-bottom: var(--space-5);
	}

	.crumb {
		background: none;
		border: none;
		padding: 2px 4px;
		cursor: pointer;
		color: var(--kv-accent);
		font-family: inherit;
		font-size: inherit;
		border-radius: var(--radius-control);
		transition: opacity 120ms linear;
	}

	.crumb:hover {
		opacity: 0.75;
	}

	.current {
		color: var(--kv-ink);
		padding: 2px 4px;
	}

	.sep {
		color: var(--kv-faint);
		user-select: none;
		font-size: 10px;
	}
</style>
