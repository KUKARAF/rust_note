// Body-fat % from a single skinfold, using the lookup chart that ships with the
// "Body Fat Caliper" (age band × skinfold-mm bucket, separate men/women tables).
//
// How the chart is used: pinch the skinfold ~2 in (5 cm) above the hipbone
// (suprailiac), read the caliper in mm, find the column for that reading and
// the row for your age. Above 36 mm add 0.25 % per extra mm.
//
// Storage: the stats format is integer-only, so a skinfold is logged as tenths
// of a mm under `skinfold.<site>` (18.5 mm → 185) and the result as tenths of a
// percent under `bodyfat` (15.2 % → 152). `TENTHS` is the shared scale.

export type Sex = 'male' | 'female';

export const TENTHS = 10;

/** Stats-metric keys the body-fat feature writes. */
export const SKINFOLD_KEY = 'skinfold'; // + `.<site>`
export const BODYFAT_KEY = 'bodyfat';

/** Upper bound (inclusive) of each age row; the last row is "56 & up". */
export const AGE_BANDS = [
	{ label: 'up to 20', max: 20 },
	{ label: '21-25', max: 25 },
	{ label: '26-30', max: 30 },
	{ label: '31-35', max: 35 },
	{ label: '36-40', max: 40 },
	{ label: '41-45', max: 45 },
	{ label: '46-50', max: 50 },
	{ label: '51-55', max: 55 },
	{ label: '56 & up', max: Infinity }
] as const;

/** Column labels: 2-3, 4-5, …, 32-33, 34-36 mm. */
export const MM_BUCKETS = [
	'2-3',
	'4-5',
	'6-7',
	'8-9',
	'10-11',
	'12-13',
	'14-15',
	'16-17',
	'18-19',
	'20-21',
	'22-23',
	'24-25',
	'26-27',
	'28-29',
	'30-31',
	'32-33',
	'34-36'
] as const;

// Transcribed from the printed chart. Rows = AGE_BANDS, columns = MM_BUCKETS.
// A couple of printed cells look odd next to their neighbours (men 41-45 @20-21
// = 24.7, men 56+ @16-17 = 23.5); they are kept as printed.
const MEN: number[][] = [
	[
		2.0, 3.9, 6.2, 8.5, 10.5, 12.5, 14.3, 16.0, 17.5, 18.9, 20.2, 21.3, 22.3, 23.1, 23.8, 24.3, 24.9
	],
	[
		2.5, 4.9, 7.3, 9.5, 11.6, 13.6, 15.4, 17.0, 18.6, 20.0, 21.2, 22.3, 23.3, 24.2, 24.9, 25.4, 25.8
	],
	[
		3.5, 6.0, 8.4, 10.6, 12.7, 14.6, 16.4, 18.1, 19.6, 21.0, 22.3, 23.4, 24.3, 25.2, 25.9, 26.5,
		26.9
	],
	[
		4.5, 7.1, 9.4, 11.7, 13.7, 15.7, 17.5, 19.2, 20.7, 22.1, 23.4, 24.5, 25.5, 26.3, 27.0, 27.5,
		28.0
	],
	[
		5.6, 8.1, 10.5, 12.7, 14.8, 16.8, 18.6, 20.2, 21.8, 23.2, 24.4, 25.6, 26.5, 27.4, 28.1, 28.6,
		29.0
	],
	[
		6.7, 9.2, 11.5, 13.8, 15.9, 17.8, 19.6, 21.3, 22.8, 24.7, 25.5, 26.6, 27.6, 28.4, 29.1, 29.7,
		30.1
	],
	[
		7.7, 10.2, 12.6, 14.8, 16.9, 18.8, 20.7, 22.4, 23.9, 25.3, 26.6, 27.7, 28.7, 29.5, 30.2, 30.7,
		31.2
	],
	[
		8.8, 11.3, 13.7, 15.9, 18.0, 20.0, 21.8, 23.4, 25.0, 26.4, 27.6, 28.7, 29.7, 30.6, 31.2, 31.8,
		32.2
	],
	[
		9.9, 12.4, 14.8, 17.0, 19.1, 21.0, 22.8, 23.5, 26.0, 27.4, 28.7, 29.8, 30.8, 31.6, 32.3, 32.9,
		33.3
	]
];

const WOMEN: number[][] = [
	[
		11.3, 13.5, 15.7, 17.7, 19.7, 21.5, 23.2, 24.8, 26.2, 27.7, 29.0, 30.2, 31.3, 32.3, 33.1, 33.9,
		34.6
	],
	[
		11.9, 14.2, 16.3, 18.4, 20.3, 22.1, 23.8, 25.5, 27.0, 28.4, 29.6, 30.8, 31.9, 32.9, 33.8, 34.5,
		35.2
	],
	[
		12.5, 14.8, 16.9, 19.0, 20.9, 22.7, 24.5, 26.1, 27.6, 29.0, 30.3, 31.5, 32.5, 33.5, 34.4, 35.2,
		35.8
	],
	[
		13.2, 15.4, 17.6, 19.6, 21.5, 23.4, 25.1, 26.7, 28.2, 29.6, 30.9, 32.1, 33.2, 34.1, 35.0, 35.8,
		36.4
	],
	[
		13.8, 16.0, 18.2, 20.2, 22.2, 24.0, 25.7, 27.3, 28.8, 30.2, 31.5, 32.7, 33.8, 34.8, 35.6, 36.4,
		37.0
	],
	[
		14.4, 16.7, 18.8, 20.8, 22.8, 24.6, 26.3, 27.9, 29.4, 30.8, 32.1, 33.3, 34.4, 35.4, 36.3, 37.0,
		37.7
	],
	[
		15.0, 17.3, 19.4, 21.5, 23.4, 25.2, 26.9, 28.6, 30.1, 31.5, 32.8, 34.0, 35.0, 36.0, 36.9, 37.6,
		38.3
	],
	[
		15.6, 17.9, 20.0, 22.1, 24.0, 25.9, 27.6, 29.2, 30.7, 32.1, 33.4, 34.6, 35.6, 36.6, 37.5, 38.3,
		38.9
	],
	[
		16.3, 18.5, 20.7, 22.7, 24.6, 26.5, 28.2, 29.8, 31.3, 32.7, 34.0, 35.2, 36.3, 37.2, 38.1, 38.9,
		39.5
	]
];

export const CHART: Record<Sex, number[][]> = { male: MEN, female: WOMEN };

/** Extra body-fat points per mm above the chart's 36 mm limit. */
export const OVER_36_PER_MM = 0.25;

export function ageBandIndex(age: number): number {
	const i = AGE_BANDS.findIndex((b) => age <= b.max);
	return i === -1 ? AGE_BANDS.length - 1 : i;
}

/** Column index for a reading in mm: 2-3 → 0, 4-5 → 1, …, 32-33 → 15, 34+ → 16. */
export function mmBucketIndex(mm: number): number {
	if (mm >= 34) return 16;
	return Math.max(0, Math.floor((mm - 2) / 2));
}

/**
 * Body-fat % (one decimal) for a skinfold in mm, or null when the inputs are
 * unusable. Readings below 2 mm use the first column; readings above 36 mm
 * add {@link OVER_36_PER_MM} per extra mm on top of the 34-36 column.
 */
export function bodyFatPercent(sex: Sex, age: number, skinfoldMm: number): number | null {
	if (!Number.isFinite(skinfoldMm) || skinfoldMm <= 0) return null;
	if (!Number.isFinite(age) || age < 0) return null;
	const base = CHART[sex][ageBandIndex(age)][mmBucketIndex(skinfoldMm)];
	const extra = skinfoldMm > 36 ? (skinfoldMm - 36) * OVER_36_PER_MM : 0;
	return Math.round((base + extra) * 10) / 10;
}

/** Whole years between an ISO birth date and `on` (both `YYYY-MM-DD`), or null. */
export function ageOn(birthDate: string, on: string): number | null {
	const b = /^(\d{4})-(\d{2})-(\d{2})$/.exec(birthDate);
	const o = /^(\d{4})-(\d{2})-(\d{2})$/.exec(on);
	if (!b || !o) return null;
	let age = Number(o[1]) - Number(b[1]);
	if (Number(o[2]) < Number(b[2]) || (Number(o[2]) === Number(b[2]) && Number(o[3]) < Number(b[3])))
		age -= 1;
	return age >= 0 ? age : null;
}

// ---- Jackson-Pollock 3-site -------------------------------------------------
//
// Sum the three skinfolds (mm), convert to body density with the age-adjusted
// quadratic, then to body fat % with the Siri equation. Sites differ by sex.

export type Jp3Site = 'chest' | 'abdomen' | 'thigh' | 'triceps' | 'suprailiac';

export const JP3_SITES: Record<Sex, Jp3Site[]> = {
	male: ['chest', 'abdomen', 'thigh'],
	female: ['triceps', 'suprailiac', 'thigh']
};

/** Body density: `c0 − c1·S + c2·S² − c3·age`. */
const JP3_COEFFS: Record<Sex, [number, number, number, number]> = {
	male: [1.10938, 0.0008267, 0.0000016, 0.0002574],
	female: [1.0994921, 0.0009929, 0.0000023, 0.0001392]
};

/** Siri equation: body density (g/cc) → body fat %. */
export function siri(density: number): number {
	return 495 / density - 450;
}

/**
 * Jackson-Pollock 3-site body-fat % (one decimal) from the sex's three
 * skinfolds in mm, or null when a site is missing/non-positive or age is invalid.
 */
export function jackson3Percent(
	sex: Sex,
	age: number,
	sites: Partial<Record<Jp3Site, number>>
): number | null {
	if (!Number.isFinite(age) || age < 0) return null;
	let sum = 0;
	for (const site of JP3_SITES[sex]) {
		const v = sites[site];
		if (v === undefined || !Number.isFinite(v) || v <= 0) return null;
		sum += v;
	}
	const [c0, c1, c2, c3] = JP3_COEFFS[sex];
	const pct = siri(c0 - c1 * sum + c2 * sum * sum - c3 * age);
	return Number.isFinite(pct) ? Math.round(pct * 10) / 10 : null;
}

export interface MonthlyBodyFat {
	month: string; // YYYY-MM
	/** Last reading of the month, in percent. */
	value: number;
	/** Date of that reading. */
	date: string;
	/** Number of readings that month. */
	count: number;
}

/** Collapse daily `bodyfat` values (tenths of %) to one point per month (last reading wins). */
export function monthlyBodyFat(days: { date: string; value: number }[]): MonthlyBodyFat[] {
	const sorted = [...days].sort((a, b) => a.date.localeCompare(b.date));
	const out = new Map<string, MonthlyBodyFat>();
	for (const d of sorted) {
		const month = d.date.slice(0, 7);
		const prev = out.get(month);
		out.set(month, {
			month,
			value: d.value / TENTHS,
			date: d.date,
			count: (prev?.count ?? 0) + 1
		});
	}
	return [...out.values()];
}
