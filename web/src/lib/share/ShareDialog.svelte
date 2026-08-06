<script lang="ts">
	// Share-link management dialog for the note owner: create a link (view or
	// edit access, with an expiry) and manage existing links for this note.
	//
	// Backend contract (see crates/server/src/share/mod.rs):
	//   POST   /api/shares                 { note_id, permission, expires_in } -> ShareLinkResponse
	//   GET    /api/shares?note_id={id}    -> ShareLinkResponse[]
	//   DELETE /api/shares/{token}         -> 204
	//
	// The backend's `url` field is built from ITS OWN base_url (the backend's
	// own host:port), which is not necessarily where the guest page is served
	// from (frontend origin != backend origin in dev, and possibly in prod).
	// We ignore it and build the user-facing link ourselves from
	// `window.location.origin`, which is always correct for "wherever this
	// app is being served from right now".
	import { apiGet, apiPost, apiDelete, ApiError } from '$lib/api/client';
	import Card from '$lib/design/Card.svelte';
	import Button from '$lib/design/Button.svelte';
	import Chip from '$lib/design/Chip.svelte';
	import Input from '$lib/design/Input.svelte';
	import Toast from '$lib/design/Toast.svelte';

	type Permission = 'view' | 'edit';
	type ExpiresIn = '24h' | '14d' | 'never';
	type ShareStatus = 'active' | 'expired';

	interface ShareLink {
		token: string;
		note_id: string;
		permission: Permission;
		created_at: string;
		expires_at: string | null;
		status: ShareStatus;
		url: string;
	}

	let {
		noteId,
		noteTitle,
		onClose
	}: {
		noteId: string;
		noteTitle: string;
		onClose: () => void;
	} = $props();

	let permission = $state<Permission>('view');
	let expiresIn = $state<ExpiresIn>('24h');
	let creating = $state(false);
	let createError = $state<string | null>(null);

	// The most recently created link, shown as a copyable field.
	let newLink = $state<ShareLink | null>(null);
	let toastMessage = $state<string | null>(null);
	let toastTimer: ReturnType<typeof setTimeout> | undefined;

	let links = $state<ShareLink[]>([]);
	let listState = $state<'loading' | 'ok' | 'forbidden' | 'error'>('loading');
	let listErrorMessage = $state<string | null>(null);
	let revoking = $state<Record<string, boolean>>({});

	function frontendUrl(token: string): string {
		return `${window.location.origin}/shared/${token}`;
	}

	function showToast(message: string) {
		toastMessage = message;
		clearTimeout(toastTimer);
		toastTimer = setTimeout(() => (toastMessage = null), 2400);
	}

	async function loadLinks() {
		listState = 'loading';
		listErrorMessage = null;
		try {
			links = await apiGet<ShareLink[]>(`/api/shares?note_id=${encodeURIComponent(noteId)}`);
			listState = 'ok';
		} catch (err) {
			if (err instanceof ApiError && err.status === 403) {
				listState = 'forbidden';
				return;
			}
			listState = 'error';
			listErrorMessage = err instanceof Error ? err.message : 'Failed to load share links';
		}
	}

	async function createLink() {
		creating = true;
		createError = null;
		try {
			const created = await apiPost<ShareLink>('/api/shares', {
				note_id: noteId,
				permission,
				expires_in: expiresIn
			});
			newLink = created;
			await loadLinks();
		} catch (err) {
			if (err instanceof ApiError && err.status === 403) {
				createError = "You don't have permission to share this note.";
			} else {
				createError = err instanceof Error ? err.message : 'Failed to create link';
			}
		} finally {
			creating = false;
		}
	}

	async function copyLink(token: string) {
		try {
			await navigator.clipboard.writeText(frontendUrl(token));
			showToast('link copied');
		} catch {
			showToast('copy failed — select and copy manually');
		}
	}

	async function revokeLink(token: string) {
		revoking = { ...revoking, [token]: true };
		try {
			await apiDelete(`/api/shares/${encodeURIComponent(token)}`);
			if (newLink?.token === token) newLink = null;
			await loadLinks();
		} catch (err) {
			listErrorMessage = err instanceof Error ? err.message : 'Failed to revoke link';
		} finally {
			const rest = { ...revoking };
			delete rest[token];
			revoking = rest;
		}
	}

	function formatExpiry(link: ShareLink): string {
		if (link.status === 'expired') return 'expired';
		if (!link.expires_at) return 'never expires';
		const d = new Date(link.expires_at);
		if (Number.isNaN(d.getTime())) return 'unknown expiry';
		return `expires ${d.toLocaleString()}`;
	}

	function onKeydown(event: KeyboardEvent) {
		if (event.key === 'Escape') onClose();
	}

	function onBackdropClick(event: MouseEvent) {
		if (event.target === event.currentTarget) onClose();
	}

	loadLinks();
</script>

<svelte:window onkeydown={onKeydown} />

<div class="share-backdrop" onclick={onBackdropClick} role="presentation">
	<div class="share-dialog-wrap">
		<Card>
			<div class="share-dialog">
				<div class="share-dialog-header">
					<div class="share-dialog-title-group">
						<span class="share-dialog-eyebrow">&gt; share link</span>
						<h2 class="share-dialog-title">{noteTitle}</h2>
					</div>
					<button class="share-dialog-close" onclick={onClose} aria-label="Close">✕</button>
				</div>

				<section class="share-section">
					<span class="share-section-title">Access</span>
					<div class="share-radio-row">
						<button
							type="button"
							class="share-radio"
							class:selected={permission === 'view'}
							onclick={() => (permission = 'view')}
						>
							<span class="share-radio-label">View</span>
							<span class="share-radio-desc">anyone with the link can view live</span>
						</button>
						<button
							type="button"
							class="share-radio"
							class:selected={permission === 'edit'}
							onclick={() => (permission = 'edit')}
						>
							<span class="share-radio-label">Edit</span>
							<span class="share-radio-desc">anyone with the link can edit live</span>
						</button>
					</div>
				</section>

				<section class="share-section">
					<span class="share-section-title">Expires</span>
					<div class="share-chip-row">
						{#each [['24h', '24H'], ['14d', '14D'], ['never', 'NEVER']] as [value, label] (value)}
							<button
								type="button"
								class="share-chip-btn"
								onclick={() => (expiresIn = value as ExpiresIn)}
							>
								<Chip
									color={expiresIn === value ? 'accent' : 'dim'}
									variant={expiresIn === value ? 'tint' : 'outline'}
								>
									{label}
								</Chip>
							</button>
						{/each}
					</div>
				</section>

				{#if createError}
					<p class="share-error">{createError}</p>
				{/if}

				<Button variant="primary" size="md" onclick={createLink} disabled={creating}>
					{creating ? 'Creating…' : 'Create link'}
				</Button>

				{#if newLink}
					<div class="share-new-link">
						<Input
							value={frontendUrl(newLink.token)}
							label="Link"
							readonly
							onclick={(e) => (e.target as HTMLInputElement).select()}
						/>
						<Button variant="outline" size="sm" onclick={() => copyLink(newLink!.token)}>
							Copy link
						</Button>
					</div>
				{/if}

				{#if toastMessage}
					<Toast message={toastMessage} showCursor={false} />
				{/if}

				<section class="share-section">
					<span class="share-section-title">Active links</span>
					{#if listState === 'loading'}
						<p class="share-hint">loading…</p>
					{:else if listState === 'forbidden'}
						<p class="share-hint warn">You don't have permission to manage links for this note.</p>
					{:else if listState === 'error'}
						<p class="share-hint danger">{listErrorMessage}</p>
					{:else if links.length === 0}
						<p class="share-hint">No share links yet.</p>
					{:else}
						<ul class="share-link-list">
							{#each links as link (link.token)}
								<li class="share-link-row">
									<div class="share-link-meta">
										<Chip
											color={link.permission === 'edit' ? 'orange' : 'accent'}
											variant="outline"
										>
											{link.permission}
										</Chip>
										<span class="share-link-token">…{link.token.slice(-8)}</span>
										<span class="share-link-expiry" class:danger={link.status === 'expired'}>
											{formatExpiry(link)}
										</span>
									</div>
									<div class="share-link-actions">
										<button
											type="button"
											class="share-link-copy"
											onclick={() => copyLink(link.token)}
											title="Copy link"
										>
											⧉
										</button>
										<button
											type="button"
											class="share-link-revoke"
											onclick={() => revokeLink(link.token)}
											disabled={revoking[link.token]}
											title="Revoke"
										>
											✕
										</button>
									</div>
								</li>
							{/each}
						</ul>
					{/if}
				</section>
			</div>
		</Card>
	</div>
</div>

<style>
	.share-backdrop {
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

	.share-dialog-wrap {
		width: 100%;
		max-width: 30rem;
	}

	.share-dialog {
		display: flex;
		flex-direction: column;
		gap: var(--space-7);
	}

	.share-dialog-header {
		display: flex;
		align-items: flex-start;
		justify-content: space-between;
		gap: var(--space-4);
	}

	.share-dialog-title-group {
		display: flex;
		flex-direction: column;
		gap: var(--space-2);
		min-width: 0;
	}

	.share-dialog-eyebrow {
		font-family: var(--font-pixel);
		font-size: var(--type-label);
		letter-spacing: var(--tracking-pixel);
		text-transform: uppercase;
		color: var(--kv-orange);
	}

	.share-dialog-title {
		margin: 0;
		font-family: var(--font-term);
		font-size: var(--type-body);
		font-weight: normal;
		color: var(--kv-ink);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.share-dialog-close {
		flex: 0 0 auto;
		background: transparent;
		border: 1px solid var(--border-default);
		border-radius: var(--radius-control);
		color: var(--kv-dim);
		font-family: var(--font-pixel);
		font-size: var(--type-label);
		cursor: pointer;
		padding: 6px 8px;
	}

	.share-dialog-close:hover {
		color: var(--kv-ink);
		border-color: var(--border-accent);
	}

	.share-section {
		display: flex;
		flex-direction: column;
		gap: var(--space-4);
	}

	.share-section-title {
		font-family: var(--font-pixel);
		font-size: var(--type-label);
		letter-spacing: var(--tracking-pixel);
		text-transform: uppercase;
		color: var(--kv-dim);
	}

	.share-radio-row {
		display: grid;
		grid-template-columns: 1fr 1fr;
		gap: var(--space-3);
	}

	.share-radio {
		display: flex;
		flex-direction: column;
		gap: var(--space-2);
		align-items: flex-start;
		text-align: left;
		background: var(--surface-input);
		border: 1px solid var(--border-default);
		border-radius: var(--radius-control);
		padding: var(--space-4);
		cursor: pointer;
		transition:
			border-color 120ms linear,
			background 120ms linear;
	}

	.share-radio.selected {
		border-color: var(--kv-accent);
		background: color-mix(in srgb, var(--kv-accent) 8%, var(--surface-input));
	}

	.share-radio-label {
		font-family: var(--font-pixel);
		font-size: var(--type-label);
		letter-spacing: var(--tracking-pixel);
		text-transform: uppercase;
		color: var(--kv-ink);
	}

	.share-radio.selected .share-radio-label {
		color: var(--kv-accent);
	}

	.share-radio-desc {
		font-family: var(--font-term);
		font-size: var(--type-meta);
		color: var(--kv-dim);
	}

	.share-chip-row {
		display: flex;
		gap: var(--space-3);
	}

	.share-chip-btn {
		background: transparent;
		border: none;
		padding: 0;
		cursor: pointer;
	}

	.share-error {
		font-family: var(--font-term);
		font-size: var(--type-meta);
		color: var(--kv-danger);
		margin: 0;
	}

	.share-new-link {
		display: flex;
		align-items: flex-end;
		gap: var(--space-3);
	}

	.share-new-link :global(.kv-input-wrap) {
		flex: 1;
		min-width: 0;
	}

	.share-hint {
		font-family: var(--font-term);
		font-size: var(--type-meta);
		color: var(--kv-dim);
		margin: 0;
	}

	.share-hint.warn {
		color: var(--kv-orange);
	}

	.share-hint.danger {
		color: var(--kv-danger);
	}

	.share-link-list {
		list-style: none;
		margin: 0;
		padding: 0;
		display: flex;
		flex-direction: column;
		gap: var(--space-3);
	}

	.share-link-row {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: var(--space-3);
		background: var(--surface-input);
		border: 1px solid var(--border-default);
		border-radius: var(--radius-control);
		padding: var(--space-3) var(--space-4);
	}

	.share-link-meta {
		display: flex;
		align-items: center;
		gap: var(--space-3);
		min-width: 0;
		flex-wrap: wrap;
	}

	.share-link-token {
		font-family: var(--font-term);
		font-size: var(--type-meta);
		color: var(--kv-dim);
	}

	.share-link-expiry {
		font-family: var(--font-term);
		font-size: var(--type-meta);
		color: var(--kv-faint);
	}

	.share-link-expiry.danger {
		color: var(--kv-danger);
	}

	.share-link-actions {
		display: flex;
		align-items: center;
		gap: var(--space-2);
		flex: 0 0 auto;
	}

	.share-link-copy,
	.share-link-revoke {
		background: transparent;
		border: 1px solid var(--border-default);
		border-radius: var(--radius-control);
		color: var(--kv-dim);
		cursor: pointer;
		width: 26px;
		height: 26px;
		font-size: 12px;
		line-height: 1;
	}

	.share-link-copy:hover {
		color: var(--kv-accent);
		border-color: var(--border-accent);
	}

	.share-link-revoke:hover:not(:disabled) {
		color: var(--kv-danger);
		border-color: var(--kv-danger);
	}

	.share-link-revoke:disabled {
		opacity: 0.5;
		cursor: not-allowed;
	}
</style>
