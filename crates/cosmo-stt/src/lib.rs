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
//!
//! ## Tiers
//!
//! [`hotwords`], the BPE vocab derivation and [`join`] are pure and build
//! in the core tier. Everything that loads a model is behind the `sherpa`
//! feature: [`Stt`] (both models, resident), [`Session`] (one recording)
//! and [`vad`].

mod bpe;
pub mod hotwords;

#[cfg(feature = "sherpa")]
mod engine;
#[cfg(feature = "sherpa")]
pub mod model;
#[cfg(feature = "sherpa")]
mod session;
#[cfg(feature = "sherpa")]
pub mod vad;

#[cfg(feature = "sherpa")]
pub use engine::{Decoded, Loaded, Stt, SttConfig};
#[cfg(feature = "sherpa")]
pub use session::{Event, Session, Transcript};

pub use bpe::vocab_from_tokens;

use std::path::PathBuf;

/// Everything loading or decoding can report, with the fix where there is
/// one, so `doctor` can print it as is.
#[derive(Debug, thiserror::Error)]
pub enum SttError {
    #[error("model file not found: {0} (run scripts/fetch-models --asr)")]
    Missing(PathBuf),
    #[error("sherpa-onnx could not load the model in {0}")]
    Load(PathBuf),
    #[error("no speech model configured (need a streaming or an offline model)")]
    NoModel,
    #[error("could not start a recognizer thread: {0}")]
    Thread(String),
    #[error("the recognizer thread is gone")]
    Closed,
    #[cfg(feature = "sherpa")]
    #[error(transparent)]
    Vad(#[from] vad::VadError),
}

/// Segment texts → one transcript: empty segments dropped, one space
/// between the rest.
pub fn join<'a>(texts: impl IntoIterator<Item = &'a str>) -> String {
    texts
        .into_iter()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    #[test]
    fn segments_join_with_single_spaces_and_empties_vanish() {
        assert_eq!(
            super::join(["Open Firefox.", "", "  and move it ", "to workspace three."]),
            "Open Firefox. and move it to workspace three."
        );
        assert_eq!(super::join([] as [&str; 0]), "");
    }
}
