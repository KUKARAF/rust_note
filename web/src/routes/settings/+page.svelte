<script lang="ts">
	import { onMount } from 'svelte';
	import SectionTitle from '$lib/design/SectionTitle.svelte';
	import Chip from '$lib/design/Chip.svelte';
	import Button from '$lib/design/Button.svelte';
	import { settings, setTheme, type Theme } from '$lib/stores/settings';
	import { IS_APP } from '$lib/api/deviceToken';
	import {
		getMirrorState,
		chooseMirrorFolder,
		mirrorAllSyncedNotes,
		type MirrorState
	} from '$lib/app/noteMirror';

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

	// --- Notes-folder mirror (app build only, see $lib/app/noteMirror) -------

	let mirrorState = $state<MirrorState | null>(null); // null while loading
	let mirrorBusy = $state(false);
	// "X/Y" while a mirror pass runs, or the final written/failed summary.
	let mirrorProgress = $state<string | null>(null);
	let mirrorError = $state<string | null>(null);

	onMount(() => {
		if (IS_APP) void refreshMirrorState();
	});

	async function refreshMirrorState() {
		mirrorState = await getMirrorState();
	}

	function trackProgress(done: number, total: number) {
		mirrorProgress = `Mirroring ${done}/${total}...`;
	}

	async function chooseFolder() {
		if (mirrorBusy) return;
		mirrorBusy = true;
		mirrorError = null;
		mirrorProgress = null;
		try {
			if (await chooseMirrorFolder(trackProgress)) {
				mirrorProgress = null; // status line reflects the new folder
			}
			// Picker cancel: nothing changed, just drop back to the status.
		} catch (err) {
			console.error('Failed to choose mirror folder', err);
			mirrorError = 'Could not set up the folder. Please try again.';
		} finally {
			mirrorBusy = false;
			await refreshMirrorState();
		}
	}

	async function rerunMirror() {
		if (mirrorBusy) return;
		mirrorBusy = true;
		mirrorError = null;
		mirrorProgress = null;
		try {
			const { written, failed } = await mirrorAllSyncedNotes(trackProgress);
			mirrorProgress = `Done: ${written} written, ${failed} failed.`;
		} catch (err) {
			console.error('Full mirror pass failed', err);
			mirrorError = 'Mirror pass failed. Please try again.';
		} finally {
			mirrorBusy = false;
			await refreshMirrorState();
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

	{#if IS_APP}
		<section class="settings-section">
			<h2 class="settings-section-title">Notes folder</h2>
			<p class="mirror-status">
				{#if mirrorState === null}
					Loading…
				{:else if mirrorState.status === 'authorized'}
					Folder: {mirrorState.folderName}
				{:else if mirrorState.status === 'revoked'}
					Access revoked — choose the folder again
				{:else}
					Not set
				{/if}
			</p>
			<div class="mirror-actions">
				<Button variant="outline" size="sm" onclick={chooseFolder} disabled={mirrorBusy}>
					{mirrorState?.status === 'authorized' ? 'Change folder' : 'Choose folder'}
				</Button>
				{#if mirrorState?.status === 'authorized'}
					<Button variant="outline" size="sm" onclick={rerunMirror} disabled={mirrorBusy}>
						Re-run full mirror
					</Button>
				{/if}
			</div>
			{#if mirrorProgress}<p class="mirror-progress">{mirrorProgress}</p>{/if}
			{#if mirrorError}<p class="settings-error">{mirrorError}</p>{/if}
		</section>
	{/if}
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

	.mirror-status {
		font-family: var(--font-term);
		font-size: var(--type-meta);
		color: var(--kv-dim);
		margin: 0 0 var(--space-4) 0;
	}

	.mirror-actions {
		display: flex;
		gap: var(--space-3);
	}

	.mirror-progress {
		font-family: var(--font-term);
		font-size: var(--type-meta);
		color: var(--kv-dim);
		margin: var(--space-4) 0 0 0;
	}
</style>
