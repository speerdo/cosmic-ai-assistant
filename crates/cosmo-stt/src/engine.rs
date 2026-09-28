//! The two resident models, each on its own worker thread (after
//! cosmic-voice's `asr.rs`).
//!
//! - **Streaming** on `asr_threads`: fed audio as it arrives, greedy,
//!   emitting a partial whenever the text changes. Nobody waits on it.
//! - **Offline** on `offline_threads`: decodes finished segments in the
//!   order they're queued, with hotwords. Everybody waits on it, so it
//!   never shares a thread pool with the streaming model.
//!
//! Workers are plain threads fed through `std` channels; results come back
//! on tokio `sync` channels, awaitable from the daemon's runtime or blocked
//! on from a plain thread. No runtime is started here.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use sherpa_onnx::{OfflineRecognizer, OnlineRecognizer, OnlineStream};
use tokio::sync::{mpsc as tmpsc, oneshot};

use crate::SttError;
use crate::model::ModelFiles;
use crate::vad::Vad;

/// Sample rate everything here runs at (capture's rate).
pub(crate) const RATE: i32 = 16_000;

/// What to load.
#[derive(Debug, Clone)]
pub struct SttConfig {
    /// Partials model directory. `None` runs without partials.
    pub streaming: Option<PathBuf>,
    /// Commit model directory. `None` commits the streaming model's final
    /// text instead (blueprint §16: commands may not need an offline pass).
    pub offline: Option<PathBuf>,
    /// Silero model for segment cuts and the backstop.
    pub vad: PathBuf,
    pub asr_threads: i32,
    /// Capped at 4: past that the offline model stops getting faster.
    pub offline_threads: i32,
    /// How hard hotwords pull the beam search.
    pub hotwords_score: f32,
    pub segment: cosmo_audio::SegmentConfig,
}

impl SttConfig {
    /// The default model pair from `scripts/fetch-models --asr` and
    /// `--vad`, or `None` without a cache directory.
    pub fn defaults() -> Option<Self> {
        let asr = crate::model::default_asr_dir()?;
        Some(Self {
            streaming: Some(asr.join(crate::model::DEFAULT_STREAMING)),
            offline: Some(asr.join(crate::model::DEFAULT_OFFLINE)),
            vad: crate::vad::default_model_path()?,
            asr_threads: 2,
            offline_threads: 4,
            hotwords_score: 1.5,
            segment: cosmo_audio::SegmentConfig::default(),
        })
    }
}

impl SttConfig {
    /// The user's choices from `config.ron` (phase-3 keys), over the
    /// defaults. `None` without a cache directory.
    pub fn from_config(config: &cosmo_config::Config) -> Option<Self> {
        let mut out = Self::defaults()?;
        let asr = crate::model::default_asr_dir()?;
        for (name, slot) in [
            (&config.asr_streaming_model, &mut out.streaming),
            (&config.asr_offline_model, &mut out.offline),
        ] {
            match name.as_str() {
                "" => {}
                "none" => *slot = None,
                dir => *slot = Some(asr.join(dir)),
            }
        }
        out.asr_threads = i32::from(config.asr_threads);
        out.offline_threads = i32::from(config.offline_threads);
        Some(out)
    }
}

/// What loaded, for `doctor`.
#[derive(Debug, Clone)]
pub struct Loaded {
    pub name: String,
    pub load: Duration,
    /// Hotwords apply (offline only; needs a writable `bpe.vocab`).
    pub hotwords: bool,
}

/// One decoded segment.
#[derive(Debug, Clone, PartialEq)]
pub struct Decoded {
    pub text: String,
    pub audio: Duration,
    pub decode: Duration,
}

enum StreamMsg {
    Begin(u64, tmpsc::UnboundedSender<String>),
    Audio(Vec<f32>),
    /// Finish the recording. With a reply, flush and send the final text;
    /// without one, just drop the stream (the commit comes from the
    /// offline model, and a backlog here must not delay it).
    End(Option<oneshot::Sender<String>>),
}

struct Job {
    samples: Vec<f32>,
    hotwords: Arc<str>,
    reply: oneshot::Sender<Decoded>,
}

/// Both models, resident. Cheap to clone: clones share the workers, which
/// stop when the last clone is dropped.
#[derive(Clone)]
pub struct Stt {
    workers: Arc<Workers>,
    pub(crate) vad: PathBuf,
    pub(crate) segment: cosmo_audio::SegmentConfig,
    pub streaming_model: Option<Loaded>,
    pub offline_model: Option<Loaded>,
}

/// A worker thread and the channel feeding it.
struct Worker<M> {
    tx: Option<mpsc::Sender<M>>,
    thread: Option<JoinHandle<()>>,
}

struct Workers {
    streaming: Option<Worker<StreamMsg>>,
    offline: Option<Worker<Job>>,
    /// The live recording's number. Bumped at every begin and end, so the
    /// streaming thread can see that audio still queued for a recording is
    /// stale and skip it rather than decode a backlog nobody will read.
    epoch: Arc<AtomicU64>,
}

impl Drop for Workers {
    /// Stop and **join** both threads. A process must not exit while one
    /// is mid-decode: the runtime is torn down under it (seen as an ONNX
    /// Runtime kernel error at exit, findings §5).
    fn drop(&mut self) {
        self.epoch.fetch_add(1, Ordering::AcqRel);
        // Close both channels first, so the threads wind down together.
        if let Some(w) = &mut self.streaming {
            w.tx = None;
        }
        if let Some(w) = &mut self.offline {
            w.tx = None;
        }
        let threads = [
            self.streaming.as_mut().and_then(|w| w.thread.take()),
            self.offline.as_mut().and_then(|w| w.thread.take()),
        ];
        for t in threads.into_iter().flatten() {
            let _ = t.join();
        }
    }
}

impl Stt {
    fn stream_tx(&self) -> Option<&mpsc::Sender<StreamMsg>> {
        self.workers.streaming.as_ref()?.tx.as_ref()
    }

    fn offline_tx(&self) -> Option<&mpsc::Sender<Job>> {
        self.workers.offline.as_ref()?.tx.as_ref()
    }

    /// Load whatever `config` names, each model on its own thread (in
    /// parallel), and wait until both are resident. Fails naming the missing file.
    pub fn load(config: &SttConfig) -> Result<Self, SttError> {
        if config.streaming.is_none() && config.offline.is_none() {
            return Err(SttError::NoModel);
        }
        // Check the VAD up front: failing on the first key press is worse.
        Vad::new(&config.vad)?;
        let epoch = Arc::new(AtomicU64::new(0));
        let streaming = config
            .streaming
            .as_ref()
            .map(|dir| {
                let files = ModelFiles::find(dir)?;
                let threads = config.asr_threads.max(1);
                let epoch = epoch.clone();
                spawn(
                    "cosmo-asr-stream",
                    files,
                    move |f| Ok((f.online(threads)?, false)),
                    move |rec, rx| stream_loop(rec, rx, &epoch),
                )
            })
            .transpose()?;
        let offline = config
            .offline
            .as_ref()
            .map(|dir| {
                let files = ModelFiles::find(dir)?;
                let (threads, score) = (config.offline_threads.clamp(1, 4), config.hotwords_score);
                spawn(
                    "cosmo-asr-commit",
                    files,
                    move |f| f.offline(threads, score),
                    job_loop,
                )
            })
            .transpose()?;
        let (streaming, streaming_loading) = streaming.unzip();
        let (offline, offline_loading) = offline.unzip();
        // Owned from here, so an early return below still stops and joins
        // whichever thread did start.
        let workers = Arc::new(Workers {
            streaming,
            offline,
            epoch,
        });
        Ok(Self {
            streaming_model: streaming_loading.map(Loading::wait).transpose()?,
            offline_model: offline_loading.map(Loading::wait).transpose()?,
            workers,
            vad: config.vad.clone(),
            segment: config.segment,
        })
    }

    pub(crate) fn begin_stream(&self, partials: tmpsc::UnboundedSender<String>) {
        if let Some(tx) = self.stream_tx() {
            let id = self.workers.epoch.fetch_add(1, Ordering::AcqRel) + 1;
            let _ = tx.send(StreamMsg::Begin(id, partials));
        }
    }

    pub(crate) fn stream_audio(&self, samples: &[f32]) {
        if let Some(tx) = self.stream_tx() {
            let _ = tx.send(StreamMsg::Audio(samples.to_vec()));
        }
    }

    /// End the streaming recording; `flush` asks for its final text.
    pub(crate) fn end_stream(&self, flush: bool) -> Option<oneshot::Receiver<String>> {
        let tx = self.stream_tx()?;
        if !flush {
            self.workers.epoch.fetch_add(1, Ordering::AcqRel);
            let _ = tx.send(StreamMsg::End(None));
            return None;
        }
        let (reply, rx) = oneshot::channel();
        tx.send(StreamMsg::End(Some(reply))).ok()?;
        Some(rx)
    }

    pub(crate) fn decode(
        &self,
        samples: Vec<f32>,
        hotwords: Arc<str>,
    ) -> Option<oneshot::Receiver<Decoded>> {
        let tx = self.offline_tx()?;
        let (reply, rx) = oneshot::channel();
        tx.send(Job {
            samples,
            hotwords,
            reply,
        })
        .ok()?;
        Some(rx)
    }
}

/// Start a worker thread that loads its model, reports how that went, then
/// serves messages until every sender is gone. Returns at once: both
/// models load in parallel, and [`Loading::wait`] collects the outcome.
fn spawn<R, M, L, S>(
    name: &str,
    files: ModelFiles,
    load: L,
    serve: S,
) -> Result<(Worker<M>, Loading), SttError>
where
    L: FnOnce(&ModelFiles) -> Result<(R, bool), SttError> + Send + 'static,
    S: FnOnce(R, mpsc::Receiver<M>) + Send + 'static,
    M: Send + 'static,
{
    let (tx, rx) = mpsc::channel();
    let (ready_tx, ready_rx) = mpsc::channel();
    let thread = std::thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            let t = Instant::now();
            match load(&files) {
                Ok((rec, hotwords)) => {
                    let loaded = Loaded {
                        name: files.name(),
                        load: t.elapsed(),
                        hotwords,
                    };
                    tracing::info!(model = %loaded.name, load_ms = loaded.load.as_millis(), "asr model resident");
                    let _ = ready_tx.send(Ok(loaded));
                    serve(rec, rx);
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                }
            }
        })
        .map_err(|e| SttError::Thread(e.to_string()))?;
    Ok((
        Worker {
            tx: Some(tx),
            thread: Some(thread),
        },
        Loading {
            name: name.to_owned(),
            ready: ready_rx,
        },
    ))
}

/// A worker still loading its model.
struct Loading {
    name: String,
    ready: mpsc::Receiver<Result<Loaded, SttError>>,
}

impl Loading {
    fn wait(self) -> Result<Loaded, SttError> {
        self.ready
            .recv()
            .map_err(|_| SttError::Thread(format!("{} died while loading", self.name)))?
    }
}

fn stream_loop(rec: OnlineRecognizer, rx: mpsc::Receiver<StreamMsg>, epoch: &AtomicU64) {
    let mut id = 0;
    let mut current: Option<(OnlineStream, tmpsc::UnboundedSender<String>, String)> = None;
    let drain = |rec: &OnlineRecognizer, s: &OnlineStream| {
        while rec.is_ready(s) {
            rec.decode(s);
        }
        rec.get_result(s).map(|r| r.text).unwrap_or_default()
    };
    for msg in rx {
        match msg {
            // A new recording supersedes one never ended.
            StreamMsg::Begin(n, partials) => {
                id = n;
                current = Some((rec.create_stream(), partials, String::new()));
            }
            StreamMsg::Audio(samples) => {
                let Some((stream, partials, last)) = current.as_mut() else {
                    continue;
                };
                // Superseded or ended without a flush: drain, don't decode.
                if epoch.load(Ordering::Acquire) != id {
                    continue;
                }
                stream.accept_waveform(RATE, &samples);
                let text = drain(&rec, stream);
                if text != *last {
                    let _ = partials.send(text.clone());
                    *last = text;
                }
            }
            StreamMsg::End(None) => current = None,
            StreamMsg::End(Some(reply)) => {
                let text = current.take().map(|(stream, _, _)| {
                    // The model looks ahead by a chunk; pad so the last
                    // words are emitted, then flush.
                    stream.accept_waveform(RATE, &vec![0.0; RATE as usize * 6 / 10]);
                    stream.input_finished();
                    drain(&rec, &stream)
                });
                let _ = reply.send(text.unwrap_or_default().trim().to_owned());
            }
        }
    }
}

fn job_loop(rec: OfflineRecognizer, rx: mpsc::Receiver<Job>) {
    for job in rx {
        let t = Instant::now();
        let stream = if job.hotwords.is_empty() {
            rec.create_stream()
        } else {
            rec.create_stream_with_hotwords(&job.hotwords)
        };
        stream.accept_waveform(RATE, &job.samples);
        rec.decode(&stream);
        let text = stream.get_result().map(|r| r.text).unwrap_or_default();
        let _ = job.reply.send(Decoded {
            text: text.trim().to_owned(),
            audio: Duration::from_secs_f64(job.samples.len() as f64 / f64::from(RATE)),
            decode: t.elapsed(),
        });
    }
}
