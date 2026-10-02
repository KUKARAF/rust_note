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
		setOpenrouterKey,
		setNotifyEnabled,
		setNotifyScope,
		setNotifyPriority,
		setNotifySchedule,
		setNotifyEndpoint,
		setNotifyToken,
		DEFAULT_NOTIFY_ENDPOINT,
		type Theme,
		type NotifyScope,
		type NotifyPriority,
		type NotifyTestResult
	} from '$lib/stores/settings';
	import { apiPost } from '$lib/api/client';
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

	// --- AI / OpenRouter (powers the /todo natural-language query) -----------

	// Suggestions for the dropdown; any valid `vendor/model` id is accepted by
	// the backend, so this list isn't exhaustive.
	const MODEL_SUGGESTIONS = [
		'minimax/minimax-m3',
		'openai/gpt-4o-mini',
		'anthropic/claude-3.5-haiku',
		'google/gemini-flash-1.5'
	];

	let apiKeyInput = $state('');
	let aiSaving = $state(false);
	let aiError = $state<string | null>(null);
	let aiSaved = $state<string | null>(null);

	async function chooseModel(model: string) {
		if (aiSaving || model === $settings.openrouterModel) return;
		aiSaving = true;
		aiError = null;
		aiSaved = null;
		try {
			await setOpenrouterModel(model);
		} catch (err) {
			aiError = 'Could not save the model.';
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
		} catch (err) {
			aiError = 'Could not clear the API key.';
			console.error(err);
		} finally {
			aiSaving = false;
		}
	}

	// --- Notifications / priority-notify digest -------------------------------

	const SCOPE_OPTIONS: { id: NotifyScope; label: string }[] = [
		{ id: 'ours', label: 'OURS' },
		{ id: 'theirs', label: 'THEIRS' },
		{ id: 'both', label: 'BOTH' }
	];

	const PRIORITY_OPTIONS: { id: NotifyPriority; label: string }[] = [
		{ id: 'low', label: 'LOW' },
		{ id: 'medium', label: 'MEDIUM' },
		{ id: 'high', label: 'HIGH' },
		{ id: 'critical', label: 'CRITICAL' }
	];

	const SCHEDULE_PRESETS: { label: string; rrule: string }[] = [
		{ label: 'Daily at 08:00', rrule: 'FREQ=DAILY;BYHOUR=8;BYMINUTE=0' },
		{ label: 'Daily at 18:00', rrule: 'FREQ=DAILY;BYHOUR=18;BYMINUTE=0' },
		{
			label: 'Weekday mornings',
			rrule: 'FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR;BYHOUR=8;BYMINUTE=0'
		}
	];
	const CUSTOM_PRESET = 'custom';

	let notifySaving = $state(false);
	let notifyError = $state<string | null>(null);

	// Which preset the schedule picker shows as active; recomputed whenever the
	// loaded/saved RRULE changes so a value that matches no preset falls back to
	// "Custom…" and reveals the raw-RRULE input.
	let schedulePreset = $derived(
		SCHEDULE_PRESETS.find((p) => p.rrule === $settings.notifySchedule)?.rrule ?? CUSTOM_PRESET
	);
	let customScheduleInput = $state('');

	async function toggleNotifyEnabled() {
		if (notifySaving) return;
		notifySaving = true;
		notifyError = null;
		try {
			await setNotifyEnabled(!$settings.notifyEnabled);
		} catch (err) {
			notifyError = 'Could not save.';
			console.error(err);
		} finally {
			notifySaving = false;
		}
	}

	async function chooseScope(scope: NotifyScope) {
		if (notifySaving || scope === $settings.notifyScope) return;
		notifySaving = true;
		notifyError = null;
		try {
			await setNotifyScope(scope);
		} catch (err) {
			notifyError = 'Could not save the scope.';
			console.error(err);
		} finally {
			notifySaving = false;
		}
	}

	async function choosePriority(priority: NotifyPriority) {
		if (notifySaving || priority === $settings.notifyPriority) return;
		notifySaving = true;
		notifyError = null;
		try {
			await setNotifyPriority(priority);
		} catch (err) {
			notifyError = 'Could not save the priority.';
			console.error(err);
		} finally {
			notifySaving = false;
		}
	}

	async function choosePreset(rrule: string) {
		if (notifySaving || rrule === schedulePreset) return;
		if (rrule === CUSTOM_PRESET) {
			// Reveal the raw-RRULE input seeded with the current value; nothing is
			// saved until the user edits and applies it.
			customScheduleInput = $settings.notifySchedule;
			return;
		}
		notifySaving = true;
		notifyError = null;
		try {
			await setNotifySchedule(rrule);
		} catch (err) {
			notifyError = 'Could not save the schedule.';
			console.error(err);
		} finally {
			notifySaving = false;
		}
	}

	async function applyCustomSchedule() {
		if (notifySaving || customScheduleInput.trim() === '') return;
		notifySaving = true;
		notifyError = null;
		try {
			await setNotifySchedule(customScheduleInput.trim());
		} catch (err) {
			notifyError = 'Could not save the schedule.';
			console.error(err);
		} finally {
			notifySaving = false;
		}
	}

	let endpointInput = $state($settings.notifyEndpoint);

	// The store starts out with a default endpoint and is updated once
	// `GET /api/settings` resolves (see root `+layout.svelte`); seed the field
	// from the real value as soon as it arrives, but only the first time so a
	// later save elsewhere doesn't clobber an in-progress edit.
	let endpointSeeded = false;
	$effect(() => {
		if (!endpointSeeded && !$settings.loading) {
			endpointInput = $settings.notifyEndpoint;
			endpointSeeded = true;
		}
	});

	async function saveEndpoint() {
		if (notifySaving || endpointInput.trim() === '') return;
		notifySaving = true;
		notifyError = null;
		try {
			await setNotifyEndpoint(endpointInput.trim());
		} catch (err) {
			notifyError = 'Could not save the endpoint.';
			console.error(err);
		} finally {
			notifySaving = false;
		}
	}

	let notifyTokenInput = $state('');
	let notifySaved = $state<string | null>(null);

	async function saveNotifyToken() {
		if (notifySaving || notifyTokenInput.trim() === '') return;
		notifySaving = true;
		notifyError = null;
		notifySaved = null;
		try {
			await setNotifyToken(notifyTokenInput.trim());
			notifyTokenInput = '';
			notifySaved = 'API key saved.';
		} catch (err) {
			notifyError = 'Could not save the API key.';
			console.error(err);
		} finally {
			notifySaving = false;
		}
	}

	async function clearNotifyToken() {
		if (notifySaving) return;
		notifySaving = true;
		notifyError = null;
		notifySaved = null;
		try {
			await setNotifyToken('');
			notifyTokenInput = '';
			notifySaved = 'API key cleared.';
		} catch (err) {
			notifyError = 'Could not clear the API key.';
			console.error(err);
		} finally {
			notifySaving = false;
		}
	}

	let notifyTestBusy = $state(false);
	let notifyTestResult = $state<string | null>(null);

	async function sendTestNotification() {
		if (notifyTestBusy || !$settings.hasNotifyToken) return;
		notifyTestBusy = true;
		notifyTestResult = null;
		notifyError = null;
		try {
			const result = await apiPost<NotifyTestResult>('/api/settings/notify-test');
			notifyTestResult = result.sent
				? `Sent: ${result.count} item${result.count === 1 ? '' : 's'}`
				: result.title;
		} catch (err) {
			notifyTestResult = err instanceof Error ? err.message : 'Could not send the test.';
			console.error(err);
		} finally {
			notifyTestBusy = false;
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

	<section class="settings-section">
		<h2 class="settings-section-title">AI · OpenRouter</h2>
		<p class="ai-help">
			Powers the natural-language query on the <a href={resolve('/todo')}>Todos</a> board. Set a model
			and an OpenRouter API key; the key is stored on the server and never shown again.
		</p>

		<div class="ai-field">
			<span class="ai-label">Model</span>
			<div class="model-options">
				{#each MODEL_SUGGESTIONS as m (m)}
					<button
						class="theme-option"
						onclick={() => chooseModel(m)}
						disabled={aiSaving}
						aria-pressed={$settings.openrouterModel === m}
					>
						<Chip color="accent" variant={$settings.openrouterModel === m ? 'tint' : 'outline'}>
							{m}{$settings.openrouterModel === m ? ' ✓' : ''}
						</Chip>
					</button>
				{/each}
			</div>
			<p class="ai-current">Active: {$settings.openrouterModel}</p>
		</div>

		<div class="ai-field">
			<span class="ai-label">API key</span>
			<p class="ai-current">
				{$settings.hasOpenrouterKey ? 'A key is set ✓' : 'No key set'}
			</p>
			<div class="ai-key-row">
				<Input
					type="password"
					placeholder={$settings.hasOpenrouterKey ? 'Replace key…' : 'sk-or-…'}
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

	<section class="settings-section">
		<h2 class="settings-section-title">Notifications · priority-notify</h2>
		<p class="ai-help">Pushes a daily digest of overdue Pipeline items to your devices.</p>

		<div class="ai-field">
			<span class="ai-label">Enabled</span>
			<button
				class="theme-option"
				onclick={toggleNotifyEnabled}
				disabled={notifySaving}
				aria-pressed={$settings.notifyEnabled}
			>
				<Chip color="accent" variant={$settings.notifyEnabled ? 'tint' : 'outline'}>
					{$settings.notifyEnabled ? 'ON ✓' : 'OFF'}
				</Chip>
			</button>
		</div>

		<div class="ai-field">
			<span class="ai-label">Scope</span>
			<div class="model-options">
				{#each SCOPE_OPTIONS as opt (opt.id)}
					<button
						class="theme-option"
						onclick={() => chooseScope(opt.id)}
						disabled={notifySaving}
						aria-pressed={$settings.notifyScope === opt.id}
					>
						<Chip color="accent" variant={$settings.notifyScope === opt.id ? 'tint' : 'outline'}>
							{opt.label}{$settings.notifyScope === opt.id ? ' ✓' : ''}
						</Chip>
					</button>
				{/each}
			</div>
		</div>

		<div class="ai-field">
			<span class="ai-label">Priority</span>
			<div class="model-options">
				{#each PRIORITY_OPTIONS as opt (opt.id)}
					<button
						class="theme-option"
						onclick={() => choosePriority(opt.id)}
						disabled={notifySaving}
						aria-pressed={$settings.notifyPriority === opt.id}
					>
						<Chip color="accent" variant={$settings.notifyPriority === opt.id ? 'tint' : 'outline'}>
							{opt.label}{$settings.notifyPriority === opt.id ? ' ✓' : ''}
						</Chip>
					</button>
				{/each}
			</div>
		</div>

		<div class="ai-field">
			<span class="ai-label">Schedule</span>
			<div class="model-options">
				{#each SCHEDULE_PRESETS as preset (preset.rrule)}
					<button
						class="theme-option"
						onclick={() => choosePreset(preset.rrule)}
						disabled={notifySaving}
						aria-pressed={schedulePreset === preset.rrule}
					>
						<Chip color="accent" variant={schedulePreset === preset.rrule ? 'tint' : 'outline'}>
							{preset.label}{schedulePreset === preset.rrule ? ' ✓' : ''}
						</Chip>
					</button>
				{/each}
				<button
					class="theme-option"
					onclick={() => choosePreset(CUSTOM_PRESET)}
					disabled={notifySaving}
					aria-pressed={schedulePreset === CUSTOM_PRESET}
				>
					<Chip color="accent" variant={schedulePreset === CUSTOM_PRESET ? 'tint' : 'outline'}>
						Custom…{schedulePreset === CUSTOM_PRESET ? ' ✓' : ''}
					</Chip>
				</button>
			</div>
			{#if schedulePreset === CUSTOM_PRESET}
				<div class="ai-key-row">
					<Input placeholder="FREQ=DAILY;BYHOUR=8;BYMINUTE=0" bind:value={customScheduleInput} />
					<Button variant="primary" size="sm" onclick={applyCustomSchedule} disabled={notifySaving}>
						Apply
					</Button>
				</div>
			{/if}
			<p class="ai-current">Active: {$settings.notifySchedule}</p>
		</div>

		<div class="ai-field">
			<span class="ai-label">Endpoint</span>
			<div class="ai-key-row">
				<Input placeholder={DEFAULT_NOTIFY_ENDPOINT} bind:value={endpointInput} />
				<Button variant="primary" size="sm" onclick={saveEndpoint} disabled={notifySaving}>
					Save
				</Button>
			</div>
		</div>

		<div class="ai-field">
			<span class="ai-label">API key</span>
			<p class="ai-current">
				{$settings.hasNotifyToken ? 'A key is set ✓' : 'No key set'}
			</p>
			<div class="ai-key-row">
				<Input
					type="password"
					placeholder={$settings.hasNotifyToken ? 'Replace key…' : 'Priority-notify token…'}
					bind:value={notifyTokenInput}
				/>
				<Button variant="primary" size="sm" onclick={saveNotifyToken} disabled={notifySaving}>
					Save key
				</Button>
				{#if $settings.hasNotifyToken}
					<Button variant="outline" size="sm" onclick={clearNotifyToken} disabled={notifySaving}>
						Clear
					</Button>
				{/if}
			</div>
		</div>

		<div class="ai-field">
			<Button
				variant="outline"
				size="sm"
				onclick={sendTestNotification}
				disabled={notifyTestBusy || !$settings.hasNotifyToken}
			>
				Send test
			</Button>
			{#if notifyTestResult}<p class="ai-current">{notifyTestResult}</p>{/if}
		</div>

		{#if notifySaved}<p class="ai-saved">{notifySaved}</p>{/if}
		{#if notifyError}<p class="settings-error">{notifyError}</p>{/if}
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

	.theme-options,
	.model-options {
		display: flex;
		flex-wrap: wrap;
		gap: var(--space-3);
	}

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
</style>
