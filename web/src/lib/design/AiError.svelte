<script lang="ts">
	// An AI feature failure: the server's actual reason, plus the way to fix it.
	import { resolve } from '$app/paths';

	let { message, settingsLink = true }: { message: string; settingsLink?: boolean } = $props();

	// The API's 400 bodies carry a "bad request: " prefix that means nothing here.
	const text = $derived(message.replace(/^bad request: /, ''));
</script>

{text}
{#if settingsLink}
	<a class="ai-error-link" href={`${resolve('/settings')}#ai`}>Open AI settings →</a>
{/if}

<style>
	.ai-error-link {
		color: var(--kv-accent);
		white-space: nowrap;
	}
</style>
