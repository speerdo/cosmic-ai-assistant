//! The half-duplex hook (invariant #8), delivered ahead of the microphone.
//!
//! Playback publishes "speaking" here; phase 3's capture gate reads it at
//! the ring buffer and holds the mic shut while [`SpeechGate::mic_open`] is
//! false. Nothing reads it yet — there is no mic until phase 3 — but the
//! writer side is live, so the seam is proven rather than promised.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// How long the mic stays shut after the last sample drains, so the room's
/// tail (and the device's own latency) is not transcribed as a command.
pub const SETTLE: Duration = Duration::from_millis(350);

/// Cheap, clonable view of whether cosmo is currently audible.
///
/// Lock-free: capture will consult it from its RT callback.
#[derive(Debug, Clone)]
pub struct SpeechGate {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    epoch: Instant,
    speaking: AtomicBool,
    /// Nanoseconds after `epoch` at which speech last drained; `u64::MAX`
    /// until it first has.
    quiet_since: AtomicU64,
}

impl Default for SpeechGate {
    fn default() -> Self {
        Self::new()
    }
}

impl SpeechGate {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Inner {
                epoch: Instant::now(),
                speaking: AtomicBool::new(false),
                quiet_since: AtomicU64::new(u64::MAX),
            }),
        }
    }

    /// True from the moment a clip is admitted until its audio drains.
    pub fn is_speaking(&self) -> bool {
        self.inner.speaking.load(Ordering::Acquire)
    }

    /// Whether the mic may listen at `now`: not speaking, and at least
    /// `settle` since the last drain.
    pub fn mic_open(&self, now: Instant, settle: Duration) -> bool {
        if self.is_speaking() {
            return false;
        }
        let quiet = self.inner.quiet_since.load(Ordering::Acquire);
        if quiet == u64::MAX {
            return true;
        }
        let since_epoch = now.saturating_duration_since(self.inner.epoch);
        let quiet = Duration::from_nanos(quiet);
        since_epoch.saturating_sub(quiet) >= settle
    }

    // Writers are the player's; without the backend only tests call them.
    #[cfg_attr(not(feature = "pipewire-backend"), allow(dead_code))]
    pub(crate) fn set_speaking(&self) {
        self.inner.speaking.store(true, Ordering::Release);
    }

    #[cfg_attr(not(feature = "pipewire-backend"), allow(dead_code))]
    pub(crate) fn set_quiet(&self, now: Instant) {
        let nanos = now.saturating_duration_since(self.inner.epoch).as_nanos();
        let nanos = u64::try_from(nanos).unwrap_or(u64::MAX - 1);
        self.inner.quiet_since.store(nanos, Ordering::Release);
        self.inner.speaking.store(false, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_before_anything_was_said() {
        let gate = SpeechGate::new();
        assert!(gate.mic_open(Instant::now(), SETTLE));
    }

    #[test]
    fn shut_while_speaking_and_through_the_settle() {
        let gate = SpeechGate::new();
        gate.set_speaking();
        let t0 = Instant::now();
        assert!(!gate.mic_open(t0, SETTLE));

        gate.set_quiet(t0);
        assert!(!gate.mic_open(t0 + Duration::from_millis(100), SETTLE));
        assert!(gate.mic_open(t0 + SETTLE, SETTLE));
    }
}
