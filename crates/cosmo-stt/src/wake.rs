//! The wake word (phase-7 spec §7.1), as pure pieces: which transcripts
//! count as a wake, and when a stretch of speech gets checked.
//!
//! The design (phase-7 plan, decisions): no always-on recognizer. The VAD
//! watches the mic while idle; each new stretch of speech has its first
//! ~1.5 s decoded **once**, unbiased, by the offline model; a transcript
//! that *starts* with the wake phrase turns that recording into a command.

/// Words that may come before the wake phrase ("hey cosmo").
const GREETINGS: &[&str] = &["hey", "hi", "ok", "okay", "hello"];

/// How many leading words of `transcript` are the wake phrase (with an
/// optional greeting), or `None` if it doesn't start with it. Only the
/// start counts: "…said Cosmo…" mid-sentence is not a wake, which is the
/// first guard against false accepts.
pub fn wake_prefix(transcript: &str, phrase: &str) -> Option<usize> {
    let phrase = crate::score::normalize(phrase);
    if phrase.is_empty() {
        return None;
    }
    let words = crate::score::normalize(transcript);
    let greeting = usize::from(
        words
            .first()
            .is_some_and(|w| GREETINGS.contains(&w.as_str())),
    );
    (words.get(greeting..greeting + phrase.len()) == Some(&phrase[..]))
        .then_some(greeting + phrase.len())
}

/// The transcript with its wake prefix removed ("Hey Cosmo, pause the
/// music." → "pause the music."): what's left is the command. `None` if
/// it didn't start with the wake phrase.
pub fn strip_wake(transcript: &str, phrase: &str) -> Option<String> {
    let n = wake_prefix(transcript, phrase)?;
    // Drop `n` words of the original text, keeping the rest verbatim.
    let mut rest = transcript.trim_start();
    for _ in 0..n {
        let end = rest.find(|c: char| c.is_whitespace()).unwrap_or(rest.len());
        rest = rest[end..].trim_start();
    }
    Some(
        rest.trim_start_matches([',', '.', '!', '?', ':', ';'])
            .trim()
            .to_owned(),
    )
}

/// When a stretch of speech is checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WakeConfig {
    /// Audio kept before the VAD's first speech window (its onset is late
    /// by its debounce, and a soft first syllable must survive).
    pub margin: u64,
    /// How much of a stretch is checked: the wake phrase comes first.
    pub window: u64,
    /// Silence that ends a stretch.
    pub gap: u64,
}

impl WakeConfig {
    /// At 16 kHz: 300 ms margin, 1.5 s window, 400 ms gap.
    pub const DEFAULT: Self = Self {
        margin: 4_800,
        window: 24_000,
        gap: 6_400,
    };
}

/// What the tracker wants done.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WakeCheck {
    /// Decode ring samples `[from, to)` and see if they start with the
    /// wake phrase.
    Check { from: u64, to: u64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Waiting for speech.
    Quiet,
    /// Speech began at `onset`; collecting its first window.
    Collecting { onset: u64 },
    /// Checked (or checking): no more checks until this stretch ends.
    Done,
}

/// Feeds on VAD decisions (as `Segmenter` does) and asks for **one** check
/// per stretch of speech.
#[derive(Debug, Clone)]
pub struct WakeTracker {
    config: WakeConfig,
    phase: Phase,
    /// Where the current silence run began, if in one.
    silence_from: Option<u64>,
}

impl WakeTracker {
    pub fn new(config: WakeConfig) -> Self {
        Self {
            config,
            phase: Phase::Quiet,
            silence_from: None,
        }
    }

    /// Feed the decision for the window `[start, start + len)`.
    pub fn push(&mut self, start: u64, len: usize, speech: bool) -> Option<WakeCheck> {
        let end = start + len as u64;
        if speech {
            self.silence_from = None;
        } else {
            self.silence_from.get_or_insert(start);
        }
        let ended = self
            .silence_from
            .is_some_and(|from| end - from >= self.config.gap);
        match self.phase {
            Phase::Quiet if speech => {
                let onset = start.saturating_sub(self.config.margin);
                self.phase = Phase::Collecting { onset };
                None
            }
            Phase::Quiet => None,
            Phase::Collecting { onset } => {
                // The window is full, or the stretch ended first (a short
                // "Hey Cosmo."): check what there is.
                if end - onset >= self.config.window || ended {
                    self.phase = Phase::Done;
                    let to = if ended {
                        self.silence_from.unwrap_or(end)
                    } else {
                        end
                    };
                    return Some(WakeCheck::Check { from: onset, to });
                }
                None
            }
            Phase::Done if ended => {
                self.phase = Phase::Quiet;
                None
            }
            Phase::Done => None,
        }
    }

    /// Forget any stretch in progress (a recording took over the mic).
    pub fn reset(&mut self) {
        self.phase = Phase::Quiet;
        self.silence_from = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_transcript_that_starts_with_the_phrase_wakes() {
        let w = |t: &str| wake_prefix(t, "cosmo");
        assert_eq!(w("Hey Cosmo, pause the music."), Some(2));
        assert_eq!(w("Cosmo open Firefox"), Some(1));
        assert_eq!(w("OK, Cosmo."), Some(2));
        assert_eq!(w("cosmo"), Some(1));
        assert_eq!(w("I told Cosmo to stop"), None, "not at the start");
        assert_eq!(w("Hey, how are you"), None);
        assert_eq!(w("Cosmos is a great show"), None, "a different word");
        assert_eq!(w(""), None);
        assert_eq!(wake_prefix("hey jarvis", "jarvis"), Some(2), "configurable");
    }

    #[test]
    fn the_command_is_what_follows() {
        let s = |t: &str| strip_wake(t, "cosmo");
        assert_eq!(
            s("Hey Cosmo, pause the music."),
            Some("pause the music.".into())
        );
        assert_eq!(s("Cosmo open Firefox"), Some("open Firefox".into()));
        assert_eq!(
            s("Hey Cosmo."),
            Some(String::new()),
            "a wake with no command"
        );
        assert_eq!(s("pause the music"), None);
    }

    const W: usize = 512;

    fn run(pattern: &str) -> Vec<(usize, WakeCheck)> {
        let mut t = WakeTracker::new(WakeConfig::DEFAULT);
        pattern
            .chars()
            .enumerate()
            .filter_map(|(i, c)| {
                t.push((i * W) as u64 + 100_000, W, c == 'S')
                    .map(|a| (i, a))
            })
            .collect()
    }

    #[test]
    fn a_long_stretch_is_checked_once_on_its_first_window() {
        // 10 quiet windows, then 200 speech windows (6.4 s of talk).
        let pattern = format!("{}{}", ".".repeat(10), "S".repeat(200));
        let checks = run(&pattern);
        assert_eq!(checks.len(), 1, "{checks:?}");
        let WakeCheck::Check { from, to } = checks[0].1;
        let onset = 100_000 + 10 * W as u64;
        assert_eq!(from, onset - 4_800, "the margin reaches back");
        let span = to - from;
        assert!(
            (24_000..24_000 + W as u64).contains(&span),
            "the 1.5 s window (to the next VAD window): {span}"
        );
    }

    #[test]
    fn a_short_stretch_is_checked_when_it_ends() {
        // "Hey Cosmo." (0.8 s), then silence.
        let pattern = format!("{}{}{}", ".".repeat(5), "S".repeat(25), ".".repeat(20));
        let checks = run(&pattern);
        let [(_, WakeCheck::Check { from, to })] = checks[..] else {
            panic!("{checks:?}")
        };
        assert_eq!(to, 100_000 + 30 * W as u64, "up to where the silence began");
        assert_eq!(from, 100_000 + 5 * W as u64 - 4_800);
    }

    #[test]
    fn the_next_stretch_gets_its_own_check_after_a_gap() {
        let pattern = format!(
            "{}{}{}{}",
            "S".repeat(60),
            ".".repeat(5), // under the gap: same stretch
            "S".repeat(20),
            ".".repeat(15) + &"S".repeat(60)
        );
        assert_eq!(run(&pattern).len(), 2);
    }
}
