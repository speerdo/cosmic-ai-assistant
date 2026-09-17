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
//!
//! Layout (spec §2.2): the `provider` module holds the trait and its value
//! types ([`VoiceProvider`], [`Voice`], [`Accent`], [`LatencyClass`]), `pcm`
//! the canonical mono buffer plus WAV encode/decode ([`Pcm`]), `registry`
//! provider construction ([`Registry`], [`ProviderInit`]). The modules are
//! private — everything public is re-exported at the crate root. Built-in
//! providers and the cache land in §2.4–§2.6; playback is `cosmo-audio`
//! (§2.3).

mod pcm;
mod provider;
mod registry;

pub use pcm::Pcm;
pub use provider::{Accent, Gender, LatencyClass, Voice, VoiceProvider};
pub use registry::{ProviderFactory, ProviderInit, Registry};

/// Errors surfaced by the voice layer. Providers map their specifics into
/// [`TtsError::Synthesis`]; the structured variants here are what `doctor`
/// and the CLI render.
#[derive(Debug, thiserror::Error)]
pub enum TtsError {
    #[error("unknown voice provider \"{name}\" (available: {available})")]
    UnknownProvider { name: String, available: String },
    #[error("provider \"{provider}\" has no voice \"{voice}\"")]
    UnknownVoice { provider: String, voice: String },
    #[error("no API key — run `cosmo auth login`")]
    NoKey,
    #[error("synthesis failed: {0}")]
    Synthesis(String),
    #[error("wav: {0}")]
    Wav(String),
}
