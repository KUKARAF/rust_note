<script lang="ts">
	import { resolve } from '$app/paths';
</script>

<div class="index-page">
	<h1 class="index-title">rust_note</h1>
	<p class="index-desc">
		Self-hosted, markdown-based notetaking. Go to <a href={resolve('/notes')}>Notes</a>.
	</p>
</div>

<style>
	.index-page {
		max-width: 40rem;
		margin: 0 auto;
	}

	.index-title {
		font-family: var(--font-pixel);
		font-size: var(--type-display);
		text-transform: uppercase;
		letter-spacing: var(--tracking-pixel);
		color: var(--kv-accent);
		text-shadow: var(--glow-accent);
	}

	.index-desc {
		font-family: var(--font-term);
		font-size: var(--type-body);
		color: var(--kv-dim);
	}

	.index-desc a {
		color: var(--kv-accent);
	}
</style>
