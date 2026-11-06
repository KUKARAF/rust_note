<script lang="ts">
	// Body fat — one form. Enter the skinfolds (mm); Jackson-Pollock 3-site gives
	// the body-fat %, and everything is appended to the daily note's YAML
	// frontmatter (`skinfold.<site>`, `bodyfat`, in tenths — see bodyfat.ts).
	// /stats charts the `bodyfat` series. Sex + birth date come from settings.
	import { onMount } from 'svelte';
	import { goto } from '$app/navigation';
	import { resolve } from '$app/paths';
	import { apiGet, apiPost, apiPut, ApiError } from '$lib/api/client';
	import { flagLoginRequired } from '$lib/stores/auth';
	import { settings, setBodyProfile, type BodySex } from '$lib/stores/settings';
	import { toast } from '$lib/design/toasts';
	import Card from '$lib/design/Card.svelte';
	import Button from '$lib/design/Button.svelte';
	import Input from '$lib/design/Input.svelte';
	import {
		BODYFAT_KEY,
		JP3_SITES,
		SKINFOLD_KEY,
		TENTHS,
		ageOn,
		jackson3Percent,
		type Jp3Site
	} from '$lib/stats/bodyfat';
	import type { MetricDef } from '$lib/stats/stats';

	/** Where to pinch (right side, thumb and finger lift the fold, caliper 1 cm away). */
	const SITE_HINT: Record<Jp3Site, string> = {
		chest: 'diagonal fold, halfway between armpit fold and nipple',
		abdomen: 'vertical fold, 2 cm beside the navel',
		thigh: 'vertical fold, front of thigh, halfway between hip crease and kneecap',
		triceps: 'vertical fold, back of arm, halfway between shoulder tip and elbow',
		suprailiac: 'diagonal fold, just above the hip bone'
	};

	const today = new Date().toLocaleDateString('en-CA'); // YYYY-MM-DD, local day

	let sex = $state<BodySex | ''>('');
	let birthDate = $state('');
	let date = $state(today);
	let values = $state<Record<string, string>>({});
	let saving = $state(false);
	let error = $state<string | null>(null);

	// Seed from the saved profile once settings have loaded.
	let seeded = false;
	$effect(() => {
		if (seeded || $settings.loading) return;
		seeded = true;
		sex = $settings.sex;
		birthDate = $settings.birthDate;
	});

	const sites = $derived(sex === '' ? [] : JP3_SITES[sex]);
	const mm = $derived(
		Object.fromEntries(
			Object.entries(values).map(([k, v]) => [k, Number(v.replace(',', '.'))])
		) as Partial<Record<Jp3Site, number>>
	);
	const age = $derived(birthDate ? ageOn(birthDate, date) : null);
	const result = $derived(sex === '' || age === null ? null : jackson3Percent(sex, age, mm));

	onMount(() => {
		// Fail early (and cleanly) if the session has expired.
		apiGet('/api/stats/registry').catch(async (err) => {
			if (err instanceof ApiError && err.status === 401) {
				flagLoginRequired();
				await goto(resolve('/login'));
			}
		});
	});

	async function ensureRegistry() {
		const have = new Set((await apiGet<MetricDef[]>('/api/stats/registry')).map((d) => d.metric));
		if (have.has(BODYFAT_KEY)) return;
		await apiPut(`/api/stats/registry/${BODYFAT_KEY}`, {
			label: 'Body fat',
			unit: '×0.1 %',
			chart: 'line',
			agg: 'last'
		});
	}

	async function save() {
		if (result === null || sex === '') return;
		saving = true;
		error = null;
		try {
			if (sex !== $settings.sex || birthDate !== $settings.birthDate)
				await setBodyProfile({ sex, birthDate });
			await ensureRegistry();
			for (const site of sites)
				await apiPost('/api/stats', {
					key: `${SKINFOLD_KEY}.${site}`,
					value: Math.round(mm[site]! * TENTHS),
					date
				});
			await apiPost('/api/stats', { key: BODYFAT_KEY, value: Math.round(result * TENTHS), date });
			toast(`Saved ${result.toFixed(1)} % for ${date}`);
			values = {};
		} catch (err) {
			if (err instanceof ApiError && err.status === 401) {
				flagLoginRequired();
				await goto(resolve('/login'));
				return;
			}
			error = err instanceof Error ? err.message : 'Failed to save';
		} finally {
			saving = false;
		}
	}
</script>

<div class="page">
	<Card>
		<form
			onsubmit={(e) => {
				e.preventDefault();
				void save();
			}}
		>
			<div class="row">
				<label class="sel">
					<span class="lbl">Sex</span>
					<select bind:value={sex}>
						<option value="" disabled>Select…</option>
						<option value="male">Male</option>
						<option value="female">Female</option>
					</select>
				</label>
				<Input type="date" label="Birth date" bind:value={birthDate} />
				<Input type="date" label="Date" bind:value={date} />
			</div>

			{#each sites as site (site)}
				<div class="site">
					<Input
						type="text"
						label="{site} (mm)"
						placeholder="0"
						bind:value={() => values[site] ?? '', (v) => (values[site] = v)}
					/>
					<span class="hint">{SITE_HINT[site]}</span>
				</div>
			{/each}

			<div class="out" aria-live="polite">
				{#if result !== null}
					<span class="big">{result.toFixed(1)}%</span>
					<span class="hint">Jackson-Pollock 3-site, age {age}</span>
				{:else}
					<span class="hint">
						{sex === '' || !birthDate
							? 'Set sex and birth date, then enter the skinfolds.'
							: 'Enter all three skinfolds.'}
					</span>
				{/if}
			</div>

			<Button type="submit" disabled={result === null || saving}>
				{saving ? 'Saving…' : 'Save'}
			</Button>
			{#if error}<p class="err">{error}</p>{/if}
		</form>
	</Card>
</div>

<style>
	.page {
		max-width: 520px;
		margin: 0 auto;
		padding: var(--space-4);
	}
	form {
		display: flex;
		flex-direction: column;
		gap: var(--space-4);
	}
	.row {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(140px, 1fr));
		gap: var(--space-3);
	}
	.sel {
		display: flex;
		flex-direction: column;
		gap: var(--space-2);
	}
	.lbl {
		font-family: var(--font-pixel);
		font-size: var(--type-label);
		text-transform: uppercase;
		letter-spacing: var(--tracking-pixel);
		color: var(--kv-dim);
	}
	select {
		font: inherit;
		padding: var(--space-2);
		background: transparent;
		color: inherit;
		border: 1px solid var(--kv-dim);
		min-height: 40px;
	}
	.site {
		display: flex;
		flex-direction: column;
		gap: var(--space-1);
	}
	.hint {
		font-size: 12px;
		color: var(--kv-dim);
	}
	.out {
		display: flex;
		align-items: baseline;
		gap: var(--space-3);
	}
	.big {
		font-family: var(--font-pixel);
		font-size: 28px;
	}
	.err {
		color: var(--kv-red, #d0453f);
		margin: 0;
	}
</style>
