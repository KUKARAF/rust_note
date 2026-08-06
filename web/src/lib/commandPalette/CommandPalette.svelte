<script lang="ts">
	// Placeholder command palette component.
	//
	// Will be triggered by Ctrl+K (keyboard-first UI), listing/searching
	// commands and notes with fuzzy matching, and dispatching actions
	// (navigate to note, create note, log out, etc).
	let { open = false }: { open?: boolean } = $props();
</script>

{#if open}
	<div class="command-palette-placeholder" role="dialog" aria-label="Command palette">
		<p>Command palette placeholder</p>
	</div>
{/if}

<style>
	.command-palette-placeholder {
		position: fixed;
		top: calc(var(--safe-top) + 20%);
		left: 50%;
		transform: translateX(-50%);
		border: 1px solid #999;
		background: white;
		padding: 1rem;
		min-width: 20rem;
	}
</style>
