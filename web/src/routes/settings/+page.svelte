<script lang="ts">
	import { resolve } from '$app/paths';
	import { onMount } from 'svelte';
	import SectionTitle from '$lib/design/SectionTitle.svelte';
	import Chip from '$lib/design/Chip.svelte';
	import Button from '$lib/design/Button.svelte';
	import Input from '$lib/design/Input.svelte';
	import {
		settings,
		setTheme,
		setOpenrouterModel,
		setAiCommandModel,
		setOpenrouterKey,
		setAiEndpoint,
		fetchAiModels,
		DEFAULT_AI_ENDPOINT,
		type Theme
	} from '$lib/stores/settings';
	import { apiGet } from '$lib/api/client';
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

	// --- AI / LiteLLM (powers the /todo natural-language query) --------------

	let apiKeyInput = $state('');
	let aiSaving = $state(false);
	let aiError = $state<string | null>(null);
	let aiSaved = $state<string | null>(null);

	let aiEndpointInput = $state($settings.aiEndpoint);

	// Seed-once pattern: the store starts out
	// with a default and is overwritten once `GET /api/settings` resolves, so
	// the field is seeded from the real value the first time it's available.
	let aiEndpointSeeded = false;
	$effect(() => {
		if (!aiEndpointSeeded && !$settings.loading) {
			aiEndpointInput = $settings.aiEndpoint;
			aiEndpointSeeded = true;
		}
	});

	let modelInput = $state($settings.openrouterModel);
	let modelSeeded = false;
	$effect(() => {
		if (!modelSeeded && !$settings.loading) {
			modelInput = $settings.openrouterModel;
			modelSeeded = true;
		}
	});

	// Command model — the same picker/list, but a separate field + setting
	// (`ai_command_model`) that only drives `#AI!` inline note edits.
	let commandModelInput = $state($settings.aiCommandModel);
	let commandModelSeeded = false;
	$effect(() => {
		if (!commandModelSeeded && !$settings.loading) {
			commandModelInput = $settings.aiCommandModel;
			commandModelSeeded = true;
		}
	});

	// Model list for the type-ahead picker, loaded from the LiteLLM proxy via
	// the backend (never fetched directly — the key lives server-side only).
	let aiModels = $state<string[]>([]);
	let aiModelsError = $state<string | null>(null);
	let aiModelsLoading = $state(false);
	// Guards the initial auto-fetch so it only runs once the real settings
	// have arrived, not on every store update.
	let aiModelsAutoFetched = false;

	async function loadAiModels() {
		if (!$settings.hasOpenrouterKey) {
			aiModels = [];
			aiModelsError = null;
			return;
		}
		aiModelsLoading = true;
		try {
			const result = await fetchAiModels();
			aiModels = result.models;
			aiModelsError = result.error;
		} catch (err) {
			aiModels = [];
			aiModelsError = 'Could not load the model list.';
			console.error(err);
		} finally {
			aiModelsLoading = false;
		}
	}

	$effect(() => {
		if (!aiModelsAutoFetched && !$settings.loading) {
			aiModelsAutoFetched = true;
			if ($settings.hasOpenrouterKey) void loadAiModels();
		}
	});

	async function chooseModel(model: string) {
		const trimmed = model.trim();
		if (aiSaving || trimmed === '' || trimmed === $settings.openrouterModel) return;
		aiSaving = true;
		aiError = null;
		aiSaved = null;
		try {
			await setOpenrouterModel(trimmed);
		} catch (err) {
			aiError = 'Could not save the model.';
			console.error(err);
		} finally {
			aiSaving = false;
		}
	}

	async function chooseCommandModel(model: string) {
		const trimmed = model.trim();
		if (aiSaving || trimmed === '' || trimmed === $settings.aiCommandModel) return;
		aiSaving = true;
		aiError = null;
		aiSaved = null;
		try {
			await setAiCommandModel(trimmed);
		} catch (err) {
			aiError = 'Could not save the command model.';
			console.error(err);
		} finally {
			aiSaving = false;
		}
	}

	async function saveAiEndpoint() {
		if (aiSaving || aiEndpointInput.trim() === '') return;
		aiSaving = true;
		aiError = null;
		aiSaved = null;
		try {
			await setAiEndpoint(aiEndpointInput.trim());
			aiSaved = 'Endpoint saved.';
			await loadAiModels();
		} catch (err) {
			aiError = 'Could not save the endpoint.';
			console.error(err);
		} finally {
			aiSaving = false;
		}
	}

	async function saveKey() {
		if (aiSaving || apiKeyInput.trim() === '') return;
		aiSaving = true;
		aiError = null;
		aiSaved = null;
		try {
			await setOpenrouterKey(apiKeyInput.trim());
			apiKeyInput = '';
			aiSaved = 'API key saved.';
			await loadAiModels();
		} catch (err) {
			aiError = 'Could not save the API key.';
			console.error(err);
		} finally {
			aiSaving = false;
		}
	}

	async function clearKey() {
		if (aiSaving) return;
		aiSaving = true;
		aiError = null;
		aiSaved = null;
		try {
			await setOpenrouterKey('');
			apiKeyInput = '';
			aiSaved = 'API key cleared.';
			await loadAiModels();
		} catch (err) {
			aiError = 'Could not clear the API key.';
			console.error(err);
		} finally {
			aiSaving = false;
		}
	}

	// --- Notes-folder mirror (app build only, see $lib/app/noteMirror) -------

	let mirrorState = $state<MirrorState | null>(null); // null while loading
	let mirrorBusy = $state(false);
	// "X/Y" while a mirror pass runs, or the final written/failed summary.
	let mirrorProgress = $state<string | null>(null);
	let mirrorError = $state<string | null>(null);

	// Running build sha, baked into the image by CI; shown in the footer so it's
	// unambiguous which version is live. "dev" for local runs.
	let buildSha = $state<string | null>(null);

	onMount(() => {
		if (IS_APP) void refreshMirrorState();
		void apiGet<{ sha: string; short_sha: string }>('/api/version')
			.then((v) => {
				buildSha = v.short_sha;
			})
			.catch(() => {});
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

	<section class="settings-section">
		<h2 class="settings-section-title">AI · LiteLLM</h2>
		<p class="ai-help">
			Powers the natural-language query on the <a href={resolve('/todo')}>Todos</a> board, via your LiteLLM
			proxy. The key is stored on the server and never shown again.
		</p>

		<div class="ai-field">
			<span class="ai-label">Endpoint</span>
			<div class="ai-key-row">
				<Input placeholder={DEFAULT_AI_ENDPOINT} bind:value={aiEndpointInput} />
				<Button variant="primary" size="sm" onclick={saveAiEndpoint} disabled={aiSaving}>
					Save
				</Button>
			</div>
		</div>

		<div class="ai-field">
			<span class="ai-label">Model</span>
			{#if $settings.hasOpenrouterKey && !aiModelsError}
				<Input
					placeholder={aiModelsLoading ? 'Loading models…' : 'Search models…'}
					list="ai-model-list"
					bind:value={modelInput}
					onchange={() => chooseModel(modelInput)}
				/>
				<datalist id="ai-model-list">
					{#each aiModels as m}
						<option value={m}></option>
					{/each}
				</datalist>
			{:else}
				<p class="ai-current">{aiModelsError ?? 'Set an API key first.'}</p>
				<Input
					placeholder="vendor/model"
					bind:value={modelInput}
					onchange={() => chooseModel(modelInput)}
				/>
			{/if}
			<p class="ai-current">Active: {$settings.openrouterModel}</p>
		</div>

		<div class="ai-field">
			<span class="ai-label">Command model</span>
			<p class="ai-help">Used by <code>#AI!</code> inline edits inside notes.</p>
			{#if $settings.hasOpenrouterKey && !aiModelsError}
				<Input
					placeholder={aiModelsLoading ? 'Loading models…' : 'Search models…'}
					list="ai-model-list"
					bind:value={commandModelInput}
					onchange={() => chooseCommandModel(commandModelInput)}
				/>
			{:else}
				<p class="ai-current">{aiModelsError ?? 'Set an API key first.'}</p>
				<Input
					placeholder="vendor/model"
					bind:value={commandModelInput}
					onchange={() => chooseCommandModel(commandModelInput)}
				/>
			{/if}
			<p class="ai-current">Active: {$settings.aiCommandModel}</p>
		</div>

		<div class="ai-field">
			<span class="ai-label">API key</span>
			<p class="ai-current">
				{$settings.hasOpenrouterKey ? 'A key is set ✓' : 'No key set'}
			</p>
			<div class="ai-key-row">
				<Input
					type="password"
					placeholder={$settings.hasOpenrouterKey ? 'Replace key…' : 'LiteLLM key…'}
					bind:value={apiKeyInput}
				/>
				<Button variant="primary" size="sm" onclick={saveKey} disabled={aiSaving}>Save key</Button>
				{#if $settings.hasOpenrouterKey}
					<Button variant="outline" size="sm" onclick={clearKey} disabled={aiSaving}>Clear</Button>
				{/if}
			</div>
		</div>

		{#if aiSaved}<p class="ai-saved">{aiSaved}</p>{/if}
		{#if aiError}<p class="settings-error">{aiError}</p>{/if}
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

	<p class="build-footer">running {buildSha ?? '…'}</p>
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

	.theme-options,
	.ai-help {
		font-family: var(--font-term);
		font-size: var(--type-meta);
		color: var(--kv-dim);
		margin: 0 0 var(--space-5) 0;
	}

	.ai-help a {
		color: var(--kv-accent);
	}

	.ai-field {
		margin-bottom: var(--space-5);
	}

	.ai-label {
		display: block;
		font-family: var(--font-pixel);
		font-size: var(--type-chip);
		text-transform: uppercase;
		letter-spacing: var(--tracking-pixel);
		color: var(--kv-dim);
		margin-bottom: var(--space-3);
	}

	.ai-current {
		font-family: var(--font-term);
		font-size: var(--type-meta);
		color: var(--kv-dim);
		margin: var(--space-3) 0 0 0;
	}

	.ai-key-row {
		display: flex;
		gap: var(--space-3);
		align-items: center;
		margin-top: var(--space-3);
	}

	.ai-key-row :global(.kv-input-wrap) {
		flex: 1 1 auto;
	}

	.ai-saved {
		font-family: var(--font-term);
		font-size: var(--type-meta);
		color: var(--kv-accent);
		margin-top: var(--space-4);
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

	.build-footer {
		font-family: var(--font-term);
		font-size: var(--type-meta);
		color: var(--kv-dim);
		margin: var(--space-8) 0 0 0;
		opacity: 0.6;
	}
</style>
