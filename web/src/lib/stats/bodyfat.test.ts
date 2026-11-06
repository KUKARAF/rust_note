// Unit tests for the caliper-chart body-fat lookup. Same hand-rolled runner as
// ttsChunk.test.ts:  node --experimental-strip-types src/lib/stats/bodyfat.test.ts

import {
	AGE_BANDS,
	CHART,
	MM_BUCKETS,
	ageBandIndex,
	ageOn,
	jackson3Percent,
	siri,
	bodyFatPercent,
	mmBucketIndex,
	monthlyBodyFat
} from './bodyfat.ts';

let failures = 0;

function assert(condition: boolean, message: string): void {
	if (condition) {
		console.log(`ok - ${message}`);
	} else {
		failures += 1;
		console.error(`FAIL - ${message}`);
	}
}

// Shape: 9 age rows × 17 mm columns in both tables.
for (const sex of ['male', 'female'] as const) {
	const t = CHART[sex];
	assert(t.length === AGE_BANDS.length, `${sex}: one row per age band`);
	assert(
		t.every((r) => r.length === MM_BUCKETS.length),
		`${sex}: every row has ${MM_BUCKETS.length} columns`
	);
	// Transcription sanity: fat % never decreases as the skinfold grows or as age grows.
	let rowsOk = true;
	let colsOk = true;
	for (let r = 0; r < t.length; r++)
		for (let c = 0; c < t[r].length; c++) {
			if (c > 0 && t[r][c] < t[r][c - 1]) rowsOk = false;
			if (r > 0 && t[r][c] < t[r - 1][c]) colsOk = false;
		}
	assert(rowsOk, `${sex}: increases with skinfold along every row`);
	assert(colsOk, `${sex}: increases with age down every column`);
}

// Bucket mapping.
assert(mmBucketIndex(2) === 0 && mmBucketIndex(3.5) === 0, '2-3 mm → column 0');
assert(mmBucketIndex(4) === 1 && mmBucketIndex(5) === 1, '4-5 mm → column 1');
assert(mmBucketIndex(33.9) === 15, '33.9 mm → 32-33 column');
assert(mmBucketIndex(34) === 16 && mmBucketIndex(36) === 16, '34-36 mm → last column');
assert(mmBucketIndex(1) === 0, 'below 2 mm clamps to the first column');

// Age mapping.
assert(ageBandIndex(18) === 0 && ageBandIndex(20) === 0, 'up to 20 → row 0');
assert(ageBandIndex(21) === 1, '21 → row 1');
assert(ageBandIndex(41) === 5 && ageBandIndex(45) === 5, '41-45 → row 5');
assert(ageBandIndex(80) === 8, '56+ → last row');

// Spot checks against the printed chart.
assert(bodyFatPercent('male', 18, 2) === 2.0, 'male ≤20, 2-3 mm = 2.0');
assert(bodyFatPercent('male', 30, 21) === 21.0, 'male 26-30, 20-21 mm = 21.0');
assert(bodyFatPercent('female', 30, 21) === 29.0, 'female 26-30, 20-21 mm = 29.0');
assert(bodyFatPercent('female', 60, 35) === 39.5, 'female 56+, 34-36 mm = 39.5');
assert(bodyFatPercent('male', 33, 36) === 28.0, 'male 31-35, 36 mm = 28.0');

// Over 36 mm: +0.25 per mm.
assert(bodyFatPercent('male', 33, 40) === 29.0, 'male 31-35, 40 mm = 28.0 + 4×0.25');
assert(bodyFatPercent('male', 33, 36.5) === 28.1, 'fractional mm over 36 scaled (rounded to 0.1)');

// Invalid input.
assert(bodyFatPercent('male', 30, 0) === null, 'zero skinfold → null');
assert(bodyFatPercent('male', 30, Number.NaN) === null, 'NaN skinfold → null');
assert(bodyFatPercent('male', -1, 10) === null, 'negative age → null');

// Jackson-Pollock 3-site. Checked independently: male 30 y, S=60 → Db 1.057816 → 17.9 %.
assert(
	jackson3Percent('male', 30, { chest: 15, abdomen: 25, thigh: 20 }) === 17.9,
	'JP3 male 30y, S=60 = 17.9'
);
// Female 30 y, S=60: Db = 1.0440221 → 24.1 %.
{
	const f = jackson3Percent('female', 30, { triceps: 15, suprailiac: 25, thigh: 20 });
	assert(f !== null && f === 24.1, `JP3 female 30y, S=60 = 24.1 (got ${f})`);
}
assert(Math.abs(siri(1.05) - 21.4) < 0.1, 'Siri(1.05) ≈ 21.4');
assert(
	jackson3Percent('male', 30, { chest: 15, abdomen: 25 }) === null,
	'JP3 with a missing site → null'
);
assert(
	jackson3Percent('male', 30, { chest: 15, abdomen: 25, thigh: 20, triceps: 99 }) === 17.9,
	'JP3 ignores sites not used for that sex'
);
assert(
	jackson3Percent('male', 40, { chest: 15, abdomen: 25, thigh: 20 })! >
		jackson3Percent('male', 30, { chest: 15, abdomen: 25, thigh: 20 })!,
	'JP3: older age gives a higher estimate for the same skinfolds'
);

// Age from birth date.
assert(ageOn('1994-06-15', '2026-11-06') === 32, 'birthday already passed this year');
assert(ageOn('1994-12-15', '2026-11-06') === 31, 'birthday not yet reached');
assert(ageOn('1994-11-06', '2026-11-06') === 32, 'birthday today counts');
assert(ageOn('garbage', '2026-11-06') === null, 'bad birth date → null');

// Monthly roll-up: last reading in the month wins.
{
	const m = monthlyBodyFat([
		{ date: '2026-10-20', value: 160 },
		{ date: '2026-10-03', value: 170 },
		{ date: '2026-11-02', value: 155 }
	]);
	assert(m.length === 2, 'two months');
	assert(m[0].month === '2026-10' && m[0].value === 16.0 && m[0].count === 2, 'Oct: last wins');
	assert(m[1].month === '2026-11' && m[1].value === 15.5, 'Nov: 15.5');
}

if (failures > 0) {
	console.error(`${failures} failed`);
	process.exit(1);
}
