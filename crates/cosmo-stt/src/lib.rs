//! Speech to text via sherpa-onnx: two resident int8 models on separate
//! thread pools.
//!
//! - **Streaming** (nemotron-speech-streaming-en-0.6b class) on `asr_threads`,
//!   ~560ms chunks, drives live partials.
//! - **Offline** (parakeet-tdt-0.6b-v2 class) on `offline_threads` (cap 4),
//!   produces the committing transcript.
//!
//! **Hotword biasing is the reflex path's unlock:** bias the beam search
//! toward the ~30 command phrases plus installed app names, keyed by the
//! focused `app_id`. Segmented decoding cuts at pauses so long utterances
//! decode incrementally instead of super-linearly.

#[cfg(feature = "sherpa")]
pub mod vad;

/// Link-spike access (phase-3 spec §3.1). Replaced by this crate's own API
/// in §3.5.
#[cfg(feature = "sherpa")]
pub use sherpa_onnx;
