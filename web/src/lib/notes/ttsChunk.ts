// Pure text-chunking helper for progressive, buffered TTS playback.
//
// Rationale for splitting at all: synthesizing a whole note in one request
// means the user waits for the ENTIRE audio to render before hearing a single
// word, and a single slow/failed request kills the whole note. Chunking lets
// playback start after the first chunk (small, fast) and lets one bad chunk
// fail without losing the rest. See TtsDialog.svelte for the playback side
// (sequential <audio> element, prefetch buffer, abort/revoke).
//
// This module has NO side effects and NO dependencies — keep it that way so
// it stays trivially unit-testable (see ttsChunk.spec.ts).

/**
 * Soft cap on words per synthesized chunk. Chosen so that:
 * - each request is small enough to synthesize in roughly 1-3s on a typical
 *   TTS model, which comfortably fits inside a 2-3 chunk prefetch buffer
 *   (i.e. chunk N+2 is ready well before chunk N finishes playing at normal
 *   speech rate, ~130-160 wpm);
 * - it's still large enough (several sentences) to avoid excessive per-chunk
 *   HTTP/request overhead and overly choppy audio at sentence boundaries.
 * A single sentence longer than this is hard-split on word boundaries (see
 * `splitLongSentence`) so no chunk ever exceeds the cap.
 */
export const MAX_WORDS_PER_CHUNK = 50;

/**
 * Safety-net cap on the TOTAL number of words synthesized for one TtsDialog
 * session, across all chunks. This is deliberately generous (a few thousand
 * words is a long note, well over half an hour of speech) — it exists only to
 * stop a pathological input (an entire vault pasted as "the note") from
 * silently queuing hundreds of requests. When exceeded, `chunkText` truncates
 * and reports it via `truncated: true` rather than failing outright, so the
 * caller can synthesize the prefix and tell the user the rest was skipped.
 */
export const MAX_TOTAL_WORDS = 4000;

export interface ChunkResult {
	/** Ordered, non-empty text chunks ready to POST to /api/tts individually. */
	chunks: string[];
	/** True when the input exceeded MAX_TOTAL_WORDS and was truncated. */
	truncated: boolean;
	/** Total words found in the (untruncated) input, for messaging. */
	totalWords: number;
}

/** Collapse all whitespace runs (including newlines) to single spaces and trim. */
function normalizeWhitespace(s: string): string {
	return s.replace(/\s+/g, ' ').trim();
}

/**
 * Strip markdown markup that would otherwise be read aloud literally
 * ("hash hash My Header", "star star bold star star"). Intentionally
 * shallow — this is not a markdown parser, just the handful of markers that
 * sound jarring when spoken verbatim. Link/image syntax and tables are left
 * alone; they're rare in running prose and not worth the complexity here.
 */
function stripTrivialMarkdown(paragraph: string): string {
	return (
		paragraph
			// ATX headers: leading #'s
			.replace(/^#{1,6}\s+/, '')
			// Block quote markers
			.replace(/^>\s?/, '')
			// Bullet / numbered list markers
			.replace(/^\s*(?:[-*+]|\d+[.)])\s+/, '')
			// Bold/italic/inline-code wrappers (keep the inner text)
			.replace(/(\*\*\*|___)(.+?)\1/g, '$2')
			.replace(/(\*\*|__)(.+?)\1/g, '$2')
			.replace(/(\*|_)(.+?)\1/g, '$2')
			.replace(/`([^`]+)`/g, '$1')
	);
}

function wordCount(s: string): number {
	const trimmed = s.trim();
	if (trimmed === '') return 0;
	return trimmed.split(/\s+/).length;
}

/** Split `text` into paragraphs on blank-line boundaries. */
function splitParagraphs(text: string): string[] {
	return text
		.replace(/\r\n/g, '\n')
		.split(/\n\s*\n+/)
		.map((p) => p.trim())
		.filter((p) => p !== '');
}

/** Split a paragraph into sentences on `.`/`!`/`?` followed by whitespace (or end of string). */
function splitSentences(paragraph: string): string[] {
	const matches = paragraph.match(/[^.!?]+(?:[.!?]+(?=\s|$)|$)/g);
	if (matches === null) return [paragraph];
	return matches.map((s) => s.trim()).filter((s) => s !== '');
}

/** Hard-split a too-long sentence into word-bounded pieces of at most `maxWords` words each. */
function splitLongSentence(sentence: string, maxWords: number): string[] {
	const words = sentence.split(/\s+/).filter((w) => w !== '');
	const pieces: string[] = [];
	for (let i = 0; i < words.length; i += maxWords) {
		pieces.push(words.slice(i, i + maxWords).join(' '));
	}
	return pieces;
}

/**
 * Greedily pack sentences into chunks of at most `maxWords` words, hard-
 * splitting any single sentence that alone exceeds the cap.
 */
function chunkParagraph(paragraph: string, maxWords: number): string[] {
	if (wordCount(paragraph) <= maxWords) {
		return [paragraph];
	}

	const chunks: string[] = [];
	let current: string[] = [];
	let currentWords = 0;

	const flush = () => {
		if (current.length > 0) {
			chunks.push(current.join(' '));
			current = [];
			currentWords = 0;
		}
	};

	for (const sentence of splitSentences(paragraph)) {
		const sentenceWords = wordCount(sentence);
		if (sentenceWords > maxWords) {
			flush();
			for (const piece of splitLongSentence(sentence, maxWords)) {
				chunks.push(piece);
			}
			continue;
		}
		if (currentWords + sentenceWords > maxWords) {
			flush();
		}
		current.push(sentence);
		currentWords += sentenceWords;
	}
	flush();

	return chunks;
}

/**
 * Split `text` into ordered, synthesis-ready chunks. Pure function: no I/O,
 * no randomness, safe to unit test directly.
 */
export function chunkText(
	text: string,
	options: { maxWordsPerChunk?: number; maxTotalWords?: number } = {}
): ChunkResult {
	const maxWordsPerChunk = options.maxWordsPerChunk ?? MAX_WORDS_PER_CHUNK;
	const maxTotalWords = options.maxTotalWords ?? MAX_TOTAL_WORDS;

	const totalWords = wordCount(text.replace(/\s+/g, ' '));

	const allChunks: string[] = [];
	for (const rawParagraph of splitParagraphs(text)) {
		const paragraph = normalizeWhitespace(stripTrivialMarkdown(rawParagraph));
		if (paragraph === '') continue;
		for (const chunk of chunkParagraph(paragraph, maxWordsPerChunk)) {
			const trimmed = chunk.trim();
			if (trimmed !== '') allChunks.push(trimmed);
		}
	}

	let truncated = false;
	const chunks: string[] = [];
	let wordsSoFar = 0;
	for (const chunk of allChunks) {
		const chunkWords = wordCount(chunk);
		if (wordsSoFar + chunkWords > maxTotalWords) {
			truncated = true;
			break;
		}
		chunks.push(chunk);
		wordsSoFar += chunkWords;
	}

	return { chunks, truncated, totalWords };
}
