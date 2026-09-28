//! Pause segmentation (phase-3 spec §3.3): turns a stream of per-window
//! speech/silence decisions into **cuts** inside pauses and a **silence
//! backstop**.
//!
//! The decisions come from a VAD (Silero, in `cosmo-stt`); this half is
//! pure so the rules are testable without a model. Two jobs:
//!
//! - **Cuts.** The offline decode costs more than linearly in length, so a
//!   long utterance is split at its pauses and each finished segment
//!   decodes while the next is still being spoken. A cut is emitted as soon
//!   as a pause is long enough to trust, at a point *inside* that pause,
//!   never in speech. There is no forced cut: a long stretch with no pause
//!   just decodes as one segment.
//! - **Backstop.** A long enough silence ends the recording, so a lost key
//!   release (a keyboard that vanished without its release event reaching
//!   us, say) can't leave cosmo listening forever. It is a safety net, not
//!   end-pointing: while the key is held and events arrive, the key decides.

use std::time::Duration;

use crate::ring::CAPTURE_RATE;

/// The rules, in time rather than samples.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SegmentConfig {
    /// A silence at least this long, after speech, gets a cut. Shorter
    /// gaps are the spaces between words.
    pub min_pause: Duration,
    /// A silence this long ends the recording, whether or not anything
    /// was said before it.
    pub backstop: Duration,
}

impl Default for SegmentConfig {
    fn default() -> Self {
        Self {
            // Between-word gaps in running speech are well under this;
            // a breath between clauses is around it.
            min_pause: Duration::from_millis(400),
            // Generous: someone holding the key and thinking before they
            // speak must not be cut off. Only a lost release should hit it.
            backstop: Duration::from_secs(6),
        }
    }
}

/// What the segmenter decided after a window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegmentEvent {
    /// End the current segment at this absolute ring position and start
    /// the next one there. Always inside silence.
    Cut(u64),
    /// Silence has lasted [`SegmentConfig::backstop`]: end the recording.
    /// Emitted once per recording.
    Backstop,
}

/// Per-recording segmentation state, fed one VAD window at a time.
#[derive(Debug, Clone)]
pub struct Segmenter {
    min_pause: u64,
    backstop: u64,
    /// Absolute position of the next window's first sample.
    pos: u64,
    /// Where the current silence run began, if in one.
    silence_from: Option<u64>,
    /// Speech since the last cut (or the start): a pause after it earns a
    /// cut; a pause after nothing doesn't, so pure silence isn't split
    /// into empty segments.
    speech_since_cut: bool,
    /// The current silence run already has its cut.
    cut_this_pause: bool,
    backstop_sent: bool,
}

fn samples(d: Duration) -> u64 {
    (d.as_micros() * u128::from(CAPTURE_RATE) / 1_000_000) as u64
}

impl Segmenter {
    /// Start segmenting a recording whose first sample is at `start` in
    /// the ring.
    pub fn new(config: SegmentConfig, start: u64) -> Self {
        Self {
            min_pause: samples(config.min_pause).max(1),
            backstop: samples(config.backstop).max(1),
            pos: start,
            silence_from: Some(start),
            speech_since_cut: false,
            cut_this_pause: false,
            backstop_sent: false,
        }
    }

    /// Absolute position the next window starts at.
    pub fn position(&self) -> u64 {
        self.pos
    }

    /// Feed the VAD's decision for the next `len` samples.
    pub fn push(&mut self, len: usize, speech: bool) -> Option<SegmentEvent> {
        let end = self.pos + len as u64;
        self.pos = end;
        if speech {
            self.silence_from = None;
            self.cut_this_pause = false;
            self.speech_since_cut = true;
            return None;
        }
        let from = *self.silence_from.get_or_insert(end - len as u64);
        let run = end - from;
        if run >= self.backstop && !self.backstop_sent {
            self.backstop_sent = true;
            return Some(SegmentEvent::Backstop);
        }
        if run >= self.min_pause && self.speech_since_cut && !self.cut_this_pause {
            self.cut_this_pause = true;
            self.speech_since_cut = false;
            // The middle of the pause as known so far: the VAD's own
            // hangover already sits at its start, and the next word's
            // onset (and any pre-roll of it) at its end.
            return Some(SegmentEvent::Cut(from + self.min_pause / 2));
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: usize = 512; // Silero's window at 16 kHz: 32 ms

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    /// Feed `pattern` (`S` = speech window, `.` = silence window) and
    /// collect events with the window index they fired on.
    fn run(config: SegmentConfig, start: u64, pattern: &str) -> Vec<(usize, SegmentEvent)> {
        let mut seg = Segmenter::new(config, start);
        pattern
            .chars()
            .enumerate()
            .filter_map(|(i, c)| seg.push(W, c == 'S').map(|e| (i, e)))
            .collect()
    }

    fn config() -> SegmentConfig {
        // 320 ms = exactly 10 windows; 1.6 s = 50 windows.
        SegmentConfig {
            min_pause: ms(320),
            backstop: ms(1600),
        }
    }

    #[test]
    fn a_pause_after_speech_is_cut_in_its_middle() {
        let pattern = format!("{}{}{}", "S".repeat(20), ".".repeat(15), "S".repeat(20));
        let events = run(config(), 1000, &pattern);
        let silence = 1000 + 20 * W as u64..1000 + 35 * W as u64;
        let [(at_window, SegmentEvent::Cut(pos))] = events[..] else {
            panic!("expected one cut, got {events:?}");
        };
        assert_eq!(at_window, 29, "fires once the pause is 10 windows long");
        assert!(
            silence.contains(&pos),
            "cut {pos} outside silence {silence:?}"
        );
        assert_eq!(pos, silence.start + 5 * W as u64);
    }

    #[test]
    fn gaps_between_words_are_not_cut() {
        let pattern = "SSSSS.....SSSSS.........SSSSS".repeat(3);
        assert_eq!(run(config(), 0, &pattern), []);
    }

    #[test]
    fn one_cut_per_pause_and_none_without_speech_before_it() {
        // Leading silence (nothing said yet), two pauses, a long tail.
        let pattern = format!(
            "{}{}{}{}{}{}",
            ".".repeat(20),
            "S".repeat(5),
            ".".repeat(30),
            "S".repeat(5),
            ".".repeat(12),
            "S".repeat(5),
        );
        let cuts: Vec<_> = run(config(), 0, &pattern)
            .into_iter()
            .map(|(_, e)| e)
            .collect();
        assert_eq!(
            cuts,
            [
                SegmentEvent::Cut((25 * W + 5 * W) as u64),
                SegmentEvent::Cut((60 * W + 5 * W) as u64),
            ]
        );
    }

    #[test]
    fn backstop_ends_a_recording_once_silence_is_long_enough() {
        let mut seg = Segmenter::new(config(), 0);
        for _ in 0..10 {
            assert_eq!(seg.push(W, true), None);
        }
        let events: Vec<_> = (0..120).filter_map(|_| seg.push(W, false)).collect();
        assert_eq!(
            events,
            [
                SegmentEvent::Cut(5 * W as u64 + 10 * W as u64),
                SegmentEvent::Backstop
            ],
            "one cut, then the backstop exactly once"
        );
    }

    #[test]
    fn backstop_also_covers_a_recording_with_no_speech() {
        let events = run(config(), 0, &".".repeat(50));
        assert_eq!(events, [(49, SegmentEvent::Backstop)]);
    }

    #[test]
    fn speech_resets_the_backstop_clock() {
        let pattern = format!("{}S{}", ".".repeat(49), ".".repeat(49));
        let events = run(config(), 0, &pattern);
        assert!(
            !events.iter().any(|(_, e)| *e == SegmentEvent::Backstop),
            "49 windows either side of a word is never 50 in a row: {events:?}"
        );
    }

    #[test]
    fn windows_of_any_size_keep_positions_absolute() {
        let mut seg = Segmenter::new(config(), 7);
        seg.push(100, true);
        seg.push(1, false);
        assert_eq!(seg.position(), 108);
        // 5119 more silent samples: the pause is 5120 = 320 ms long.
        assert_eq!(seg.push(5119, false), Some(SegmentEvent::Cut(107 + 2560)));
    }
}
