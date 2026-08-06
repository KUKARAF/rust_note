<script lang="ts">
	// Presence chips for currently-connected collaborators: small squares with
	// each user's initials, bordered/tinted in their awareness color (matching
	// their live caret color). Fed from the provider's awareness states.

	export interface PresencePeer {
		/** Awareness clientID — stable per connection, used as the list key. */
		clientId: number;
		name: string;
		color: string;
		/** True for this client's own state. */
		self: boolean;
	}

	let { peers = [] }: { peers?: PresencePeer[] } = $props();

	function initials(name: string): string {
		const parts = name.trim().split(/\s+/).filter(Boolean);
		if (parts.length === 0) return '?';
		if (parts.length === 1) return parts[0].slice(0, 2).toUpperCase();
		return (parts[0][0] + parts[parts.length - 1][0]).toUpperCase();
	}
</script>

{#if peers.length > 0}
	<div class="presence" aria-label="Collaborators editing this note">
		{#each peers as peer (peer.clientId)}
			<span
				class="chip"
				class:self={peer.self}
				style="--peer-color: {peer.color};"
				title={peer.self ? `${peer.name} (you)` : peer.name}
			>
				{initials(peer.name)}
			</span>
		{/each}
	</div>
{/if}

<style>
	.presence {
		display: inline-flex;
		align-items: center;
		gap: var(--space-2);
	}

	.chip {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		min-width: 22px;
		height: 22px;
		padding: 0 3px;
		font-family: var(--font-pixel);
		font-size: var(--type-chip);
		letter-spacing: 0.04em;
		line-height: 1;
		color: var(--peer-color);
		background: color-mix(in srgb, var(--peer-color) 16%, transparent);
		border: 1.5px solid var(--peer-color);
		border-radius: var(--radius-card);
		box-shadow: 0 0 5px color-mix(in srgb, var(--peer-color) 45%, transparent);
	}

	.chip.self {
		/* subtle inner ring to mark "you" without changing the color identity */
		outline: 1px solid color-mix(in srgb, var(--peer-color) 40%, transparent);
		outline-offset: 1px;
	}
</style>
