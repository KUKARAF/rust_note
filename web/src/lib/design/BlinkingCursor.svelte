<script lang="ts">
	let {
		color = 'var(--kv-accent)'
	}: {
		color?: string;
	} = $props();
</script>

<span class="kv-blinking-cursor" style="--cursor-color: {color};">_</span>

<style>
	.kv-blinking-cursor {
		font-family: var(--font-term);
		color: var(--cursor-color);
		animation: kv-blink var(--blink-duration) step-end infinite;
	}
</style>
