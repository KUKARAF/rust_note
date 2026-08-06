<script lang="ts">
	let {
		color = 'var(--kv-accent)',
		size = 8
	}: {
		color?: string;
		size?: number;
	} = $props();
</script>

<span class="kv-pulsing-dot" style="--dot-color: {color}; --dot-size: {size}px;"></span>

<style>
	.kv-pulsing-dot {
		display: inline-block;
		width: var(--dot-size);
		height: var(--dot-size);
		border-radius: var(--radius-dot);
		background: var(--dot-color);
		animation: kv-pulse var(--pulse-duration) ease-in-out infinite alternate;
	}
</style>
