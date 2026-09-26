//! The pre-roll ring (phase-3 spec §3.2): continuous capture lands here, and
//! an utterance is a window into it, so the ~750 ms before the trigger
//! registered is still there when the key goes down.
//!
//! Lifted from cosmic-voice's `audio.rs`, and lock-free for the same
//! reason: the writer is PipeWire's RT data loop. One relaxed atomic store
//! per sample, then a release on the cursor that publishes them. Samples are
//! `AtomicU32` bit patterns rather than `f32`, so a reader racing the writer
//! at wraparound reads a stale value, not undefined behaviour. Positions are
//! absolute `u64` sample counts, so a reader that fell a whole lap behind
//! can tell, and skip the overwritten span instead of returning garbage.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

/// Capture rate. Both speech models were trained at 16 kHz; PipeWire
/// resamples from the device's rate on the way in, which is better and
/// cheaper than doing it here.
pub const CAPTURE_RATE: u32 = 16_000;

/// A single-writer, multi-reader sample ring with absolute positions.
#[derive(Debug)]
pub struct Ring {
    samples: Box<[AtomicU32]>,
    /// `capacity - 1`; capacity is a power of two.
    mask: usize,
    /// Absolute count of samples ever written.
    head: AtomicU64,
}

impl Ring {
    /// `capacity` is rounded up to a power of two.
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.max(2).next_power_of_two();
        Self {
            samples: (0..capacity).map(|_| AtomicU32::new(0)).collect(),
            mask: capacity - 1,
            head: AtomicU64::new(0),
        }
    }

    /// A ring holding `seconds` of audio at [`CAPTURE_RATE`].
    pub fn with_seconds(seconds: u32) -> Self {
        Self::new(CAPTURE_RATE as usize * seconds as usize)
    }

    pub fn capacity(&self) -> usize {
        self.mask + 1
    }

    /// Append samples. RT-safe: no locks, no allocation. Single writer.
    pub fn write(&self, chunk: &[f32]) {
        let head = self.head.load(Ordering::Relaxed);
        for (i, &s) in chunk.iter().enumerate() {
            let slot = (head as usize).wrapping_add(i) & self.mask;
            self.samples[slot].store(s.to_bits(), Ordering::Relaxed);
        }
        self.head
            .store(head + chunk.len() as u64, Ordering::Release);
    }

    /// Append `n` zero samples: gated (half-duplex) time still passes.
    pub fn write_silence(&self, n: usize) {
        let head = self.head.load(Ordering::Relaxed);
        for i in 0..n {
            let slot = (head as usize).wrapping_add(i) & self.mask;
            self.samples[slot].store(0f32.to_bits(), Ordering::Relaxed);
        }
        self.head.store(head + n as u64, Ordering::Release);
    }

    /// Absolute position of the next sample to be written.
    pub fn now(&self) -> u64 {
        self.head.load(Ordering::Acquire)
    }

    /// Where an utterance triggered now should start: `now` minus
    /// `preroll_ms`, clamped to what the ring still holds.
    pub fn mark_preroll(&self, preroll_ms: u32) -> u64 {
        let now = self.now();
        let back = u64::from(preroll_ms) * u64::from(CAPTURE_RATE) / 1000;
        now.saturating_sub(back).max(self.oldest(now))
    }

    fn oldest(&self, now: u64) -> u64 {
        now.saturating_sub(self.capacity() as u64)
    }

    /// Copy the samples in `[from, to)`, clamped to the live window. A span
    /// already overwritten is skipped rather than returned, so the result
    /// can be shorter than asked; the returned position is where it really
    /// started.
    pub fn read(&self, from: u64, to: u64) -> (u64, Vec<f32>) {
        let now = self.now();
        let to = to.min(now);
        let from = from.max(self.oldest(now)).min(to);
        let out = (from..to)
            .map(|pos| {
                f32::from_bits(self.samples[pos as usize & self.mask].load(Ordering::Relaxed))
            })
            .collect();
        (from, out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_what_was_written_with_absolute_positions() {
        let ring = Ring::new(8);
        ring.write(&[1.0, 2.0, 3.0]);
        assert_eq!(ring.now(), 3);
        assert_eq!(ring.read(0, 3), (0, vec![1.0, 2.0, 3.0]));
        assert_eq!(ring.read(1, 99), (1, vec![2.0, 3.0]), "clamped to now");
    }

    #[test]
    fn a_lapped_reader_skips_the_overwritten_span() {
        let ring = Ring::new(4);
        ring.write(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0]); // 1 and 2 are gone
        let (start, samples) = ring.read(0, 6);
        assert_eq!(start, 2);
        assert_eq!(samples, [3.0, 4.0, 5.0, 6.0]);
    }

    #[test]
    fn preroll_reaches_back_but_not_past_the_ring() {
        let ring = Ring::with_seconds(2);
        ring.write(&vec![0.5; CAPTURE_RATE as usize]); // 1 s
        assert_eq!(ring.mark_preroll(750), ring.now() - 12_000);
        assert_eq!(ring.mark_preroll(5_000), 0, "only 1 s exists");
        ring.write(&vec![0.5; 3 * CAPTURE_RATE as usize]); // 4 s total, ring holds 2
        assert_eq!(
            ring.mark_preroll(5_000),
            ring.now() - ring.capacity() as u64
        );
    }

    #[test]
    fn gated_time_is_zeros_not_a_gap() {
        let ring = Ring::new(8);
        ring.write(&[0.9]);
        ring.write_silence(2);
        ring.write(&[0.8]);
        assert_eq!(ring.read(0, 4).1, [0.9, 0.0, 0.0, 0.8]);
    }

    #[test]
    fn capacity_rounds_up_to_a_power_of_two() {
        assert_eq!(Ring::new(5).capacity(), 8);
        assert_eq!(Ring::with_seconds(1).capacity(), 16_384);
    }
}
