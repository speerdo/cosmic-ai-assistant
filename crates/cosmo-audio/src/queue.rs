//! The clip queue shared between the player's main loop and its RT callback,
//! plus the completion handle callers wait on.
//!
//! Kept free of PipeWire so the scheduling logic — gapless hand-off between
//! clips, first-audio stamping, reaping — is unit-tested in the core tier.
//!
//! The RT side ([`Queue::fill`]) only ever advances cursors: it never pops,
//! drops or allocates, so a clip's last `Arc` is always released on the main
//! loop, never on the data thread. Popping finished jobs is
//! [`Queue::take_finished`]'s job, called from the main loop.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Instant;

use tokio::sync::oneshot;

use crate::{AudioError, Clip};

/// How a clip's playback ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Every sample was handed to PipeWire and the stream drained.
    Played,
    /// Stopped before it finished — `Player::stop`, or the player shut down.
    Cancelled,
}

/// Completion handle for one enqueued clip.
#[derive(Debug)]
#[must_use = "dropping a Playback does not stop the clip; it only stops you learning when it ends"]
pub struct Playback {
    rx: oneshot::Receiver<Result<Outcome, AudioError>>,
}

impl Playback {
    pub(crate) fn failed(err: AudioError) -> Self {
        let (tx, rx) = oneshot::channel();
        let _ = tx.send(Err(err));
        Self { rx }
    }

    /// Resolves once the clip's audio has drained (or it was cancelled).
    pub async fn finished(self) -> Result<Outcome, AudioError> {
        self.rx.await.unwrap_or(Err(AudioError::Closed))
    }

    /// Blocking form of [`finished`](Self::finished) for plain threads and
    /// examples. Panics if called from inside an async runtime (tokio's
    /// `blocking_recv` rule) — use `finished().await` there.
    pub fn wait(self) -> Result<Outcome, AudioError> {
        self.rx.blocking_recv().unwrap_or(Err(AudioError::Closed))
    }
}

/// One clip in flight, with its completion sender.
#[derive(Debug)]
pub(crate) struct Job {
    pub(crate) rate: u32,
    samples: Arc<[f32]>,
    done: Option<oneshot::Sender<Result<Outcome, AudioError>>>,
    enqueued: Instant,
    first_audio: Option<Instant>,
}

impl Job {
    pub(crate) fn new(clip: &Clip, now: Instant) -> (Self, Playback) {
        let (tx, rx) = oneshot::channel();
        let job = Self {
            rate: clip.sample_rate(),
            samples: clip.shared(),
            done: Some(tx),
            enqueued: now,
            first_audio: None,
        };
        (job, Playback { rx })
    }

    /// Resolve the caller's handle. Main loop only. Emits the
    /// `speak/first_audio` bench span's measurement: enqueue → first sample
    /// handed to PipeWire.
    pub(crate) fn complete(mut self, result: Result<Outcome, AudioError>) {
        if let Some(first) = self.first_audio {
            let latency_ms = first.saturating_duration_since(self.enqueued).as_secs_f64() * 1e3;
            tracing::info_span!("speak/first_audio", latency_ms, rate = self.rate).in_scope(|| {
                tracing::debug!(outcome = ?result, "clip done");
            });
        }
        if let Some(tx) = self.done.take() {
            let _ = tx.send(result);
        }
    }
}

/// Jobs of **one** sample rate — the stream's. The player never admits a
/// job of another rate; it rebuilds the stream once this queue drains.
#[derive(Debug, Default)]
pub(crate) struct Queue {
    jobs: VecDeque<Job>,
    /// Index of the first job with samples still to write.
    head: usize,
    /// Next sample within `jobs[head]`.
    pos: usize,
    /// Set by the RT side when it has asked PipeWire to drain; cleared by
    /// the main loop when the drain completes.
    pub(crate) draining: bool,
}

impl Queue {
    pub(crate) fn with_capacity(n: usize) -> Self {
        Self {
            jobs: VecDeque::with_capacity(n),
            ..Self::default()
        }
    }

    pub(crate) fn push(&mut self, job: Job) {
        debug_assert!(self.jobs.front().is_none_or(|j| j.rate == job.rate));
        self.jobs.push_back(job);
    }

    /// No samples left to write (finished jobs may still await reaping).
    pub(crate) fn exhausted(&self) -> bool {
        self.head >= self.jobs.len()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.jobs.is_empty()
    }

    /// RT side: write up to `max_frames` samples through `put`, crossing
    /// clip boundaries without a gap. Returns how many were written.
    /// Allocation-free and drop-free.
    pub(crate) fn fill(
        &mut self,
        max_frames: usize,
        now: Instant,
        mut put: impl FnMut(f32),
    ) -> usize {
        let mut written = 0;
        while written < max_frames {
            let Some(job) = self.jobs.get_mut(self.head) else {
                break;
            };
            if job.first_audio.is_none() {
                job.first_audio = Some(now);
            }
            let take = (job.samples.len() - self.pos).min(max_frames - written);
            for &s in &job.samples[self.pos..self.pos + take] {
                put(s);
            }
            written += take;
            self.pos += take;
            if self.pos >= job.samples.len() {
                self.head += 1;
                self.pos = 0;
            }
        }
        written
    }

    /// Main loop: pop jobs whose samples have all been written.
    pub(crate) fn take_finished(&mut self) -> Vec<Job> {
        let n = self.head.min(self.jobs.len());
        self.head -= n;
        self.jobs.drain(..n).collect()
    }

    /// Main loop: empty the queue (stop / stream failure / shutdown).
    pub(crate) fn take_all(&mut self) -> Vec<Job> {
        self.head = 0;
        self.pos = 0;
        self.draining = false;
        self.jobs.drain(..).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clip(rate: u32, samples: &[f32]) -> Clip {
        Clip::new(rate, samples.to_vec()).unwrap()
    }

    fn drain(q: &mut Queue, max: usize) -> Vec<f32> {
        let mut out = Vec::new();
        q.fill(max, Instant::now(), |s| out.push(s));
        out
    }

    #[test]
    fn crosses_clip_boundaries_without_a_gap() {
        let mut q = Queue::default();
        let now = Instant::now();
        let (a, _pa) = Job::new(&clip(24_000, &[1.0, 2.0, 3.0]), now);
        let (b, _pb) = Job::new(&clip(24_000, &[4.0, 5.0]), now);
        q.push(a);
        q.push(b);

        assert_eq!(drain(&mut q, 2), [1.0, 2.0]);
        assert_eq!(drain(&mut q, 2), [3.0, 4.0]);
        assert!(!q.exhausted());
        assert_eq!(drain(&mut q, 8), [5.0]);
        assert!(q.exhausted());
        assert_eq!(drain(&mut q, 8), [] as [f32; 0]);
    }

    #[test]
    fn finished_jobs_are_reaped_in_order_and_resolve() {
        let mut q = Queue::default();
        let now = Instant::now();
        let (a, pa) = Job::new(&clip(16_000, &[0.1; 4]), now);
        let (b, pb) = Job::new(&clip(16_000, &[0.2; 4]), now);
        q.push(a);
        q.push(b);

        drain(&mut q, 6); // all of a, half of b
        let done = q.take_finished();
        assert_eq!(done.len(), 1);
        assert!(done[0].first_audio.is_some());
        for job in done {
            job.complete(Ok(Outcome::Played));
        }
        assert_eq!(pa.wait(), Ok(Outcome::Played));

        // b resumes where it left off after the reap re-indexed the queue.
        assert_eq!(drain(&mut q, 8), [0.2, 0.2]);
        assert!(q.exhausted());
        let rest = q.take_finished();
        assert_eq!(rest.len(), 1);
        assert!(q.is_empty());
        drop(rest); // sender dropped without a result
        assert_eq!(pb.wait(), Err(AudioError::Closed));
    }

    #[test]
    fn take_all_resets_cursors() {
        let mut q = Queue::default();
        let (a, _pa) = Job::new(&clip(24_000, &[1.0; 10]), Instant::now());
        q.push(a);
        drain(&mut q, 3);
        q.draining = true;
        assert_eq!(q.take_all().len(), 1);
        assert!(q.is_empty() && q.exhausted() && !q.draining);
    }

    #[test]
    fn empty_clip_finishes_without_writing() {
        let mut q = Queue::default();
        let (a, _pa) = Job::new(&clip(24_000, &[]), Instant::now());
        q.push(a);
        assert_eq!(drain(&mut q, 8), [] as [f32; 0]);
        assert!(q.exhausted());
        assert_eq!(q.take_finished().len(), 1);
    }
}
