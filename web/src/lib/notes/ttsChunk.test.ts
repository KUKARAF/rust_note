// Unit tests for the pure TTS chunker.
//
// There is no test runner (vitest/jest/etc.) configured in this project, so
// rather than add a new devDependency for a single pure-function module, this
// is a plain TypeScript script with hand-rolled assertions. Node 22.6+ can
// execute it directly via its built-in TypeScript type-stripping support:
//
//   node --experimental-strip-types src/lib/notes/ttsChunk.test.ts
//
// (also wired up as `npm run test:unit` — see package.json). It exits
// non-zero on any failed assertion, so it's CI-friable the same way a real
// test runner would be if one is ever introduced.

import { chunkText, MAX_TOTAL_WORDS, MAX_WORDS_PER_CHUNK } from './ttsChunk.ts';

let failures = 0;

function assert(condition: boolean, message: string): void {
	if (condition) {
		console.log(`ok - ${message}`);
	} else {
		failures += 1;
		console.error(`FAIL - ${message}`);
	}
}

function wordsOf(s: string): number {
	const trimmed = s.trim();
	return trimmed === '' ? 0 : trimmed.split(/\s+/).length;
}

// Empty input yields no chunks.
{
	const r = chunkText('');
	assert(r.chunks.length === 0, 'empty input produces zero chunks');
	assert(!r.truncated, 'empty input is not truncated');
	assert(r.totalWords === 0, 'empty input has zero total words');
}

// Paragraphs (blank-line separated) become separate chunks when short enough.
{
	const r = chunkText('First paragraph.\n\nSecond paragraph.');
	assert(r.chunks.length === 2, `two paragraphs -> two chunks (got ${r.chunks.length})`);
	assert(r.chunks[0] === 'First paragraph.', `chunk 0 preserved: "${r.chunks[0]}"`);
	assert(r.chunks[1] === 'Second paragraph.', `chunk 1 preserved: "${r.chunks[1]}"`);
}

// Internal whitespace/newlines within a paragraph collapse to single spaces.
{
	const r = chunkText('Hello   world\nthis   is one   paragraph.');
	assert(r.chunks.length === 1, 'single logical paragraph stays one chunk');
	assert(
		r.chunks[0] === 'Hello world this is one paragraph.',
		`whitespace normalized: "${r.chunks[0]}"`
	);
}

// Blank/whitespace-only paragraphs are dropped entirely.
{
	const r = chunkText('A.\n\n\n\n   \n\nB.');
	assert(r.chunks.length === 2, `blank paragraphs dropped (got ${r.chunks.length})`);
}

// An over-long paragraph is split on sentence boundaries, each chunk <= cap.
{
	const sentence = `${'word '.repeat(20).trim()}.`;
	const paragraph = Array.from({ length: 5 }, () => sentence).join(' ');
	const r = chunkText(paragraph, { maxWordsPerChunk: 45 });
	assert(
		r.chunks.length > 1,
		`over-long paragraph splits into multiple chunks (${r.chunks.length})`
	);
	assert(
		r.chunks.every((c) => wordsOf(c) <= 45),
		'every chunk respects the per-chunk word cap'
	);
	assert(
		r.chunks.join(' ').replace(/\s+/g, ' ') === paragraph,
		'sentence-level split preserves all text in order'
	);
}

// A single sentence longer than the cap is hard-split on word boundaries.
{
	const longSentence = `${Array.from({ length: 120 }, (_, i) => `w${i}`).join(' ')}.`;
	const r = chunkText(longSentence, { maxWordsPerChunk: 50 });
	assert(
		r.chunks.length === 3,
		`120-word sentence hard-splits into ceil(120/50)=3 (got ${r.chunks.length})`
	);
	assert(
		r.chunks.every((c) => wordsOf(c) <= 50),
		'every hard-split piece respects the word cap'
	);
	const reassembledWordCount = r.chunks.reduce((n, c) => n + wordsOf(c), 0);
	assert(
		reassembledWordCount === wordsOf(longSentence),
		'hard split preserves the total word count'
	);
}

// Trivial markdown markers are stripped so they aren't read aloud literally.
{
	const r = chunkText('## Heading\n\n- a bullet\n\nSome **bold** and _italic_ and `code`.');
	assert(r.chunks[0] === 'Heading', `header marker stripped: "${r.chunks[0]}"`);
	assert(r.chunks[1] === 'a bullet', `bullet marker stripped: "${r.chunks[1]}"`);
	assert(
		r.chunks[2] === 'Some bold and italic and code.',
		`inline markup stripped: "${r.chunks[2]}"`
	);
}

// The overall max-word safety net truncates gracefully instead of throwing.
{
	const words = `${Array.from({ length: 500 }, (_, i) => `w${i}`).join(' ')}.`;
	const r = chunkText(words, { maxWordsPerChunk: 50, maxTotalWords: 120 });
	assert(r.truncated, 'exceeding the total-word cap sets truncated=true');
	const keptWords = r.chunks.reduce((n, c) => n + wordsOf(c), 0);
	assert(keptWords <= 120, `kept chunks stay within the total-word budget (${keptWords} <= 120)`);
	assert(r.totalWords === 500, `totalWords reports the full input count (${r.totalWords})`);
}

// Exported defaults are sane, named, and ordered sensibly.
assert(
	MAX_WORDS_PER_CHUNK > 0 && MAX_WORDS_PER_CHUNK < 200,
	`MAX_WORDS_PER_CHUNK is a sane default (${MAX_WORDS_PER_CHUNK})`
);
assert(
	MAX_TOTAL_WORDS > MAX_WORDS_PER_CHUNK,
	`MAX_TOTAL_WORDS exceeds the per-chunk cap (${MAX_TOTAL_WORDS} > ${MAX_WORDS_PER_CHUNK})`
);

if (failures > 0) {
	console.error(`\n${failures} assertion(s) failed`);
	process.exit(1);
} else {
	console.log('\nall ttsChunk assertions passed');
}
