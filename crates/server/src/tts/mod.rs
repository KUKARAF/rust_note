//! Text-to-speech proxy (`POST /api/tts`).
//!
//! The AI API key lives server-side only, so the browser must not call the
//! TTS endpoint directly. This module proxies a note's (or selection's) text to
//! the user's OpenAI-compatible `audio/speech` endpoint and streams the
//! resulting `audio/wav` bytes back. See `docs/tts.md`.

pub mod routes;
