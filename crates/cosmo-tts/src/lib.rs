//! The voice layer: the pluggable `VoiceProvider` trait and the pre-rendered
//! phrase cache that makes the reflex path instant.
//!
//! Providers: Kokoro (default), Piper (weak-hardware fallback), MeloTTS (the
//! only free en-AU path — verify before promising), OpenAI TTS (steerable),
//! ElevenLabs (opt-in, paid). `Voice` carries an `accent` field so the picker
//! groups by accent.
//!
//! The phrase cache renders every canned reflex response to WAV under
//! `~/.cache/cosmo/voice/<provider>/<voice-id>/` on voice selection, so
//! reflex acks are an existing buffer push: zero synthesis latency.
