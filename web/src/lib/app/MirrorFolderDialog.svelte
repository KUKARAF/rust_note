<script lang="ts">
	// Startup prompt (app build only) asking the user to authorize a device
	// folder for the notes mirror (see $lib/app/noteMirror). Rendered by the
	// root layout the first time a logged-in user opens the app; "Not now"
	// records a dismissal so the prompt never nags again, while a plain
	// backdrop/Escape close leaves it eligible to reappear on the next launch.
	import { chooseMirrorFolder, dismissMirrorPrompt, getMirrorState } from '$lib/app/noteMirror';
	import Card from '$lib/design/Card.svelte';
	import Button from '$lib/design/Button.svelte';

	let {
		open,
		onclose
	}: {
		open: boolean;
		onclose: () => void;
	} = $props();

	// idle -> picking (system SAF picker up) -> mirroring (initial bulk pass,
	// with progress) -> done. Picker cancel drops back to idle with the dialog
	// still open, so the user can retry or explicitly decline.
	let phase = $state<'idle' | 'picking' | 'mirroring' | 'done'>('idle');
	let progressDone = $state(0);
	let progressTotal = $state(0);
	let folderName = $state<string | null>(null);
	let errorMessage = $state<string | null>(null);

	async function choose() {
		phase = 'picking';
		errorMessage = null;
		try {
			const chosen = await chooseMirrorFolder((done, total) => {
				phase = 'mirroring';
				progressDone = done;
				progressTotal = total;
			});
			if (!chosen) {
				// User backed out of the system picker — stay open, no dismissal.
				phase = 'idle';
				return;
			}
			folderName = (await getMirrorState()).folderName ?? null;
			phase = 'done';
		} catch (err) {
			console.error('Failed to set up the notes folder mirror', err);
			errorMessage = 'Could not set up the folder. Please try again.';
			phase = 'idle';
		}
	}

	function notNow() {
		dismissMirrorPrompt();
		onclose();
	}

	function onKeydown(event: KeyboardEvent) {
		// Don't allow closing mid-setup: the picker/bulk pass owns the flow.
		if (event.key === 'Escape' && (phase === 'idle' || phase === 'done')) onclose();
	}

	function onBackdropClick(event: MouseEvent) {
		if (event.target !== event.currentTarget) return;
		if (phase === 'idle' || phase === 'done') onclose();
	}
</script>

<svelte:window onkeydown={onKeydown} />

{#if open}
	<div class="mirror-backdrop" onclick={onBackdropClick} role="presentation">
		<div class="mirror-dialog-wrap">
			<Card>
				<div class="mirror-dialog">
					<div class="mirror-dialog-header">
						<span class="mirror-dialog-eyebrow">&gt; notes folder</span>
						<h2 class="mirror-dialog-title">Mirror notes to a folder</h2>
					</div>

					{#if phase === 'done'}
						<p class="mirror-copy">
							Done. Your notes are mirrored to
							<strong>{folderName ?? 'the chosen folder'}</strong> and will keep updating while the app
							is open.
						</p>
						<div class="mirror-actions">
							<Button variant="primary" size="md" onclick={onclose}>Close</Button>
						</div>
					{:else if phase === 'mirroring'}
						<p class="mirror-copy">
							Mirroring {progressDone}/{progressTotal}...
						</p>
					{:else}
						<p class="mirror-copy">
							Your notes will be saved as .md files in a folder you choose, so other apps can read
							them. You can create a new folder inside the picker. Mirroring keeps the files up to
							date while the app is open.
						</p>
						{#if errorMessage}
							<p class="mirror-error">{errorMessage}</p>
						{/if}
						<div class="mirror-actions">
							<Button variant="primary" size="md" onclick={choose} disabled={phase === 'picking'}>
								{phase === 'picking' ? 'Waiting for picker…' : 'Choose folder'}
							</Button>
							<Button variant="outline" size="md" onclick={notNow} disabled={phase === 'picking'}>
								Not now
							</Button>
						</div>
					{/if}
				</div>
			</Card>
		</div>
	</div>
{/if}

<style>
	.mirror-backdrop {
		position: fixed;
		inset: 0;
		background: rgba(3, 6, 3, 0.72);
		display: flex;
		align-items: flex-start;
		justify-content: center;
		padding: var(--space-9) var(--space-6);
		overflow-y: auto;
		z-index: 100;
	}

	.mirror-dialog-wrap {
		width: 100%;
		max-width: 26rem;
	}

	.mirror-dialog {
		display: flex;
		flex-direction: column;
		gap: var(--space-6);
	}

	.mirror-dialog-header {
		display: flex;
		flex-direction: column;
		gap: var(--space-2);
	}

	.mirror-dialog-eyebrow {
		font-family: var(--font-pixel);
		font-size: var(--type-label);
		letter-spacing: var(--tracking-pixel);
		text-transform: uppercase;
		color: var(--kv-orange);
	}

	.mirror-dialog-title {
		margin: 0;
		font-family: var(--font-term);
		font-size: var(--type-body);
		font-weight: normal;
		color: var(--kv-ink);
	}

	.mirror-copy {
		margin: 0;
		font-family: var(--font-term);
		font-size: var(--type-meta);
		color: var(--kv-dim);
		line-height: 1.5;
	}

	.mirror-copy strong {
		color: var(--kv-ink);
		font-weight: normal;
	}

	.mirror-error {
		margin: 0;
		font-family: var(--font-term);
		font-size: var(--type-meta);
		color: var(--kv-danger);
	}

	.mirror-actions {
		display: flex;
		gap: var(--space-3);
	}
</style>
