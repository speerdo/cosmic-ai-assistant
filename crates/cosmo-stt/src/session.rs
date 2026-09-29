//! One recording, from key down to transcript (phase-3 spec §3.5).
//!
//! The caller pushes 16 kHz audio as capture delivers it (from the pre-roll
//! mark on). Each push goes three ways: to the streaming model, for
//! partials; to Silero and the segmenter, for cuts; and into the current
//! segment's buffer. At each cut the finished segment is queued on the
//! offline model at once, so it decodes **while the next one is still being
//! spoken**, and by the release only the last segment is left to decode.

use std::sync::Arc;
use std::time::{Duration, Instant};

use cosmo_audio::{SegmentEvent, Segmenter};
use tokio::sync::{mpsc, oneshot};

use crate::engine::{Decoded, Stt};
use crate::vad::{Vad, WINDOW};
use crate::{SttError, join};

/// What a push can report.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// The streaming model's current text for the whole recording.
    Partial(String),
    /// A segment was cut and queued for the offline pass.
    Segment { audio: Duration },
    /// Silence has lasted the backstop: end the recording (the key's
    /// release may have been lost). Reported once.
    Backstop,
}

/// The commit.
#[derive(Debug, Clone, PartialEq)]
pub struct Transcript {
    /// The committed text: the offline segments joined, or the streaming
    /// model's final text when there is no offline model.
    pub text: String,
    /// The streaming model's text: its flushed final when it commits,
    /// otherwise the last partial seen (it isn't waited for).
    pub streaming: String,
    /// Each offline segment, in order.
    pub segments: Vec<Decoded>,
    /// From [`Session::finish`] to this transcript: release → commit.
    pub latency: Duration,
}

pub struct Session {
    stt: Stt,
    vad: Vad,
    seg: Segmenter,
    hotwords: Arc<str>,
    /// Audio since the last cut; `buf_start` is its first sample's position.
    buf: Vec<f32>,
    buf_start: u64,
    /// First and last speech heard since the last cut: `[start, end)` of the
    /// VAD windows that held it.
    speech: Option<(u64, u64)>,
    pending: Vec<oneshot::Receiver<Decoded>>,
    partials: mpsc::UnboundedReceiver<String>,
    last_partial: String,
    backstopped: bool,
}

impl Stt {
    /// Start a recording. `hotwords` is one phrase per line (see
    /// [`crate::hotwords::Hotwords::for_app`]). Only one recording streams
    /// partials at a time: starting another supersedes it.
    pub fn session(&self, hotwords: &str) -> Result<Session, SttError> {
        let (tx, partials) = mpsc::unbounded_channel();
        self.begin_stream(tx);
        Ok(Session {
            stt: self.clone(),
            vad: Vad::new(&self.vad)?,
            seg: Segmenter::new(self.segment, 0),
            hotwords: hotwords.into(),
            buf: Vec::new(),
            buf_start: 0,
            speech: None,
            pending: Vec::new(),
            partials,
            last_partial: String::new(),
            backstopped: false,
        })
    }
}

impl Session {
    /// Feed the next samples. Never blocks on a model.
    pub fn push(&mut self, samples: &[f32]) -> Vec<Event> {
        self.stt.stream_audio(samples);
        self.buf.extend_from_slice(samples);

        // (window start, speech?, what the segmenter made of it), in order.
        let mut decisions = Vec::new();
        let seg = &mut self.seg;
        self.vad.feed(samples, |speech| {
            let at = seg.position();
            decisions.push((at, speech, seg.push(WINDOW, speech)));
        });

        let mut events = Vec::new();
        for (at, speech, event) in decisions {
            if speech {
                let end = at + WINDOW as u64;
                self.speech = Some(self.speech.map_or((at, end), |(first, _)| (first, end)));
            }
            match event {
                None => {}
                Some(SegmentEvent::Cut(cut)) => {
                    let split = (cut - self.buf_start) as usize;
                    let rest = self.buf.split_off(split);
                    let segment = std::mem::replace(&mut self.buf, rest);
                    let start = std::mem::replace(&mut self.buf_start, cut);
                    let speech = self.speech.take();
                    let segment = trim(segment, start, speech);
                    events.push(Event::Segment {
                        audio: seconds(segment.len()),
                    });
                    self.queue(segment);
                }
                Some(SegmentEvent::Backstop) if !self.backstopped => {
                    self.backstopped = true;
                    events.push(Event::Backstop);
                }
                Some(SegmentEvent::Backstop) => {}
            }
        }
        while let Ok(text) = self.partials.try_recv() {
            self.last_partial.clone_from(&text);
            events.push(Event::Partial(text));
        }
        events
    }

    fn queue(&mut self, segment: Vec<f32>) {
        if let Some(rx) = self.stt.decode(segment, self.hotwords.clone()) {
            self.pending.push(rx);
        }
    }

    /// The recording is over (key released, or backstop): decode what's
    /// left and commit.
    pub async fn finish(mut self) -> Result<Transcript, SttError> {
        let t = Instant::now();
        // The tail is only worth a decode if speech was heard since the
        // last cut; trailing silence alone decodes to nothing, slowly.
        if self.seg.speech_pending() && !self.buf.is_empty() {
            let tail = std::mem::take(&mut self.buf);
            let tail = trim(tail, self.buf_start, self.speech.take());
            self.queue(tail);
        }
        // Nobody waits on a partial: with an offline model committing, the
        // streaming model is told to stop, not to catch up.
        let offline = self.stt.offline_model.is_some();
        let streaming = match self.stt.end_stream(!offline) {
            Some(rx) => rx.await.map_err(|_| SttError::Closed)?,
            None => {
                while let Ok(text) = self.partials.try_recv() {
                    self.last_partial = text;
                }
                self.last_partial.trim().to_owned()
            }
        };
        let mut segments = Vec::with_capacity(self.pending.len());
        for rx in self.pending {
            segments.push(rx.await.map_err(|_| SttError::Closed)?);
        }
        let text = if offline {
            join(segments.iter().map(|d| d.text.as_str()))
        } else {
            streaming.clone()
        };
        Ok(Transcript {
            text,
            streaming,
            segments,
            latency: t.elapsed(),
        })
    }
}

/// Silence kept either side of the speech in a segment: room for an onset
/// the VAD flagged late (its debounce is 100 ms) and a soft word ending.
const MARGIN: u64 = crate::engine::RATE as u64 * 3 / 10;

/// Cut a segment (first sample at `start`) down to its speech plus
/// [`MARGIN`]. The offline model is handed words, not the pre-roll's empty
/// lead-in or a long silent tail: with hotwords active, the beam search
/// fills unconstrained silence with them ("Zoom Zoom Zoom Firefox…",
/// findings §6a). Without a known span the segment is left whole.
fn trim(mut segment: Vec<f32>, start: u64, speech: Option<(u64, u64)>) -> Vec<f32> {
    let Some((first, last)) = speech else {
        return segment;
    };
    let end = start + segment.len() as u64;
    let lo = first.saturating_sub(MARGIN).max(start);
    let hi = (last + MARGIN).min(end).max(lo);
    segment.truncate((hi - start) as usize);
    segment.drain(..(lo - start) as usize);
    segment
}

fn seconds(samples: usize) -> Duration {
    Duration::from_secs_f64(samples as f64 / f64::from(crate::engine::RATE))
}

#[cfg(test)]
mod tests {
    use super::*;

    const M: u64 = MARGIN;

    fn numbered(start: u64, len: u64) -> Vec<f32> {
        (start..start + len).map(|i| i as f32).collect()
    }

    #[test]
    fn trim_keeps_speech_plus_a_margin_each_side() {
        let start = 1_000;
        let seg = numbered(start, 20_000);
        let out = trim(seg, start, Some((start + 8_000, start + 10_000)));
        assert_eq!(out.first().copied(), Some((start + 8_000 - M) as f32));
        assert_eq!(out.last().copied(), Some((start + 10_000 + M - 1) as f32));
    }

    #[test]
    fn trim_never_reaches_outside_the_segment() {
        let seg = numbered(0, 6_000);
        let out = trim(seg.clone(), 0, Some((100, 5_900)));
        assert_eq!(out, seg);
    }

    #[test]
    fn no_known_speech_means_no_trim() {
        let seg = numbered(0, 100);
        assert_eq!(trim(seg.clone(), 0, None), seg);
    }
}
