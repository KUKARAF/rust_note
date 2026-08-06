<script lang="ts">
	import SectionTitle from '$lib/design/SectionTitle.svelte';
	import Chip from '$lib/design/Chip.svelte';
	import { settings, setTheme, type Theme } from '$lib/stores/settings';

	// Add new entries here (and widen `Theme` in `$lib/stores/settings`) when a
	// 2nd theme ships — no other architecture change needed.
	const THEMES: { id: Theme; label: string }[] = [{ id: 'ration', label: 'RATION' }];

	let saving = $state(false);
	let error = $state<string | null>(null);

	async function choose(theme: Theme) {
		if (saving) return;
		saving = true;
		error = null;
		try {
			await setTheme(theme);
		} catch (err) {
			error = 'Could not save theme.';
			console.error(err);
		} finally {
			saving = false;
		}
	}
</script>

<div class="settings-page">
	<SectionTitle>Settings</SectionTitle>

	<section class="settings-section">
		<h2 class="settings-section-title">Theme</h2>
		<div class="theme-options">
			{#each THEMES as t (t.id)}
				<button
					class="theme-option"
					onclick={() => choose(t.id)}
					disabled={saving}
					aria-pressed={$settings.theme === t.id}
				>
					<Chip color="accent" variant={$settings.theme === t.id ? 'tint' : 'outline'}>
						{t.label}{$settings.theme === t.id ? ' ✓' : ''}
					</Chip>
				</button>
			{/each}
		</div>
		{#if error}<p class="settings-error">{error}</p>{/if}
	</section>
</div>

<style>
	.settings-page {
		max-width: 40rem;
		margin: 0 auto;
		display: flex;
		flex-direction: column;
		gap: var(--space-8);
	}

	.settings-section-title {
		font-family: var(--font-pixel);
		font-size: var(--type-label);
		text-transform: uppercase;
		letter-spacing: var(--tracking-pixel);
		color: var(--text-primary);
		margin: 0 0 var(--space-5) 0;
	}

	.theme-options {
		display: flex;
		gap: var(--space-3);
	}

	.theme-option {
		all: unset;
		cursor: pointer;
	}

	.theme-option:disabled {
		cursor: not-allowed;
		opacity: 0.6;
	}

	.settings-error {
		font-family: var(--font-term);
		font-size: var(--type-meta);
		color: var(--color-bad);
		margin-top: var(--space-4);
	}
</style>
