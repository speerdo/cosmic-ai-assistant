//! The playback input type.

use std::sync::Arc;
use std::time::Duration;

use crate::AudioError;

/// Mono `f32` samples in `[-1.0, 1.0]` at their own sample rate.
///
/// The rate is whatever the provider produced (24 kHz from Kokoro and
/// OpenAI, 22.05 kHz from most Piper voices): the stream opens at the clip's
/// rate and PipeWire's adapter resamples to the device. Resampling here
/// would be both worse and more expensive.
///
/// Samples sit behind an `Arc` so a phrase-cache entry can be played any
/// number of times without copying, and so the RT callback never frees one.
#[derive(Debug, Clone, PartialEq)]
pub struct Clip {
    sample_rate: u32,
    samples: Arc<[f32]>,
}

impl Clip {
    /// A zero rate is refused here rather than at negotiation, where it
    /// would surface as an opaque stream error.
    pub fn new(sample_rate: u32, samples: impl Into<Arc<[f32]>>) -> Result<Self, AudioError> {
        if sample_rate == 0 {
            return Err(AudioError::InvalidClip("sample rate is zero".into()));
        }
        Ok(Self {
            sample_rate,
            samples: samples.into(),
        })
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn samples(&self) -> &[f32] {
        &self.samples
    }

    pub(crate) fn shared(&self) -> Arc<[f32]> {
        self.samples.clone()
    }

    pub fn duration(&self) -> Duration {
        Duration::from_secs_f64(self.samples.len() as f64 / f64::from(self.sample_rate))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_rate_is_refused() {
        assert!(matches!(
            Clip::new(0, vec![0.0]),
            Err(AudioError::InvalidClip(_))
        ));
    }

    #[test]
    fn duration_follows_rate() {
        let clip = Clip::new(24_000, vec![0.0; 12_000]).unwrap();
        assert_eq!(clip.duration(), Duration::from_millis(500));
    }
}
