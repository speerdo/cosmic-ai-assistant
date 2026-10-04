//! Silero VAD through sherpa-onnx (phase-3 spec §3.3): one speech/silence
//! decision per 32 ms window, for `cosmo_audio::Segmenter` to turn into
//! pause cuts and the silence backstop.
//!
//! Only sherpa's frame-level `detected()` state is used. Its segment queue
//! (speech with the silence stripped out) is cleared as it fills: the
//! utterance is cut from the ring, where positions are absolute and the
//! pre-roll lives, not from copies sherpa hands back.

use std::path::{Path, PathBuf};

use sherpa_onnx::{SileroVadModelConfig, VadModelConfig, VoiceActivityDetector};

/// Samples per decision at 16 kHz. Silero is trained on 512-sample
/// windows at this rate; sherpa accepts no other size for it.
pub const WINDOW: usize = 512;

const SAMPLE_RATE: i32 = 16_000;

/// Where `scripts/fetch-models --vad` puts the model: `$XDG_CACHE_HOME` or
/// `~/.cache`, then `cosmo/models/vad/silero_vad.onnx`.
pub fn default_model_path() -> Option<PathBuf> {
    let cache = std::env::var_os("XDG_CACHE_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?;
    Some(cache.join("cosmo/models/vad/silero_vad.onnx"))
}

#[derive(Debug, thiserror::Error)]
pub enum VadError {
    #[error("VAD model not found at {0} (run `cosmo models fetch`)")]
    Missing(PathBuf),
    #[error("sherpa-onnx could not load the VAD model at {0}")]
    Load(PathBuf),
}

/// A resident Silero model with its recurrent state: one per recording
/// stream, fed windows in order.
pub struct Vad {
    inner: VoiceActivityDetector,
    /// Samples not yet making up a whole window.
    partial: Vec<f32>,
}

impl Vad {
    pub fn new(model: &Path) -> Result<Self, VadError> {
        if !model.is_file() {
            return Err(VadError::Missing(model.to_owned()));
        }
        let config = VadModelConfig {
            silero_vad: SileroVadModelConfig {
                model: Some(model.to_string_lossy().into_owned()),
                threshold: 0.5,
                // Short on purpose: these only debounce single-window
                // flicker. What counts as a *pause* is the segmenter's
                // `min_pause`, so it's one rule in one place. The silence
                // side is what a key release waits on (the release tail):
                // 100 ms made it the bulk of release → ack on the user's
                // recordings (phase-4 findings §1d), so it's 50.
                min_silence_duration: 0.05,
                min_speech_duration: 0.1,
                window_size: WINDOW as i32,
                // sherpa raises its threshold past this to force a segment
                // end. Cuts belong only in silence, so it's out of reach.
                max_speech_duration: 3600.0,
            },
            sample_rate: SAMPLE_RATE,
            num_threads: 1,
            ..Default::default()
        };
        // The buffer only holds queued segments, which are cleared after
        // every window; a few seconds is plenty.
        let inner = VoiceActivityDetector::create(&config, 5.0)
            .ok_or_else(|| VadError::Load(model.to_owned()))?;
        Ok(Self {
            inner,
            partial: Vec::with_capacity(WINDOW),
        })
    }

    /// Feed 16 kHz samples; returns one decision (`true` = speech) per
    /// whole window completed, in order. Leftover samples wait for the
    /// next call, so callers can feed whatever the ring gives them.
    pub fn feed(&mut self, samples: &[f32], mut decide: impl FnMut(bool)) {
        let mut rest = samples;
        while !rest.is_empty() {
            let take = (WINDOW - self.partial.len()).min(rest.len());
            self.partial.extend_from_slice(&rest[..take]);
            rest = &rest[take..];
            if self.partial.len() == WINDOW {
                self.inner.accept_waveform(&self.partial);
                self.partial.clear();
                self.inner.clear();
                decide(self.inner.detected());
            }
        }
    }

    /// Samples held toward the next window (fed but not yet decided): a
    /// caller tracking positions subtracts them to find where the next
    /// decision's window starts.
    pub fn pending(&self) -> usize {
        self.partial.len()
    }

    /// Forget the recurrent state and any partial window: the next sample
    /// fed starts a new recording.
    pub fn reset(&mut self) {
        self.inner.reset();
        self.partial.clear();
    }
}
