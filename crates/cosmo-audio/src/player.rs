//! The PipeWire playback stream (spec §2.3).
//!
//! One dedicated thread owns the PipeWire main loop, the connection and the
//! stream; [`Player`] is the `Send` handle the daemon holds. Commands cross
//! over a `pipewire::channel`, so the loop wakes on them without polling.
//!
//! Shape of the stream:
//!
//! - **Opened at the clip's own rate**, mono `f32`, and PipeWire's adapter
//!   resamples and upmixes to the device. A clip at another rate waits for
//!   the current run of speech to drain, then the stream is rebuilt.
//! - **`RT_PROCESS`**: the fill callback runs on PipeWire's data loop, so
//!   speech keeps playing cleanly while the machine is saturated — the same
//!   reason capture will be native in phase 3. That callback takes the queue
//!   with `try_lock` and writes one quantum of silence if it loses the race;
//!   it never blocks, allocates or frees.
//! - **Completion is drain-based**: when the queue runs dry the callback
//!   asks PipeWire to drain, and the `drained` event — all audio actually
//!   played out — is what resolves each clip's [`Playback`]. Clips queued
//!   back to back therefore resolve together, when the run of speech ends.
//! - **Inactive while idle**: after a drain the stream is deactivated, so an
//!   idle cosmo does not hold the sink awake with silence.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, mpsc};
use std::thread::JoinHandle;
use std::time::Instant;

use pipewire as pw;
use pw::properties::properties;
use pw::spa;
use spa::pod::Pod;

use crate::queue::{Job, Queue};
use crate::{AudioError, Clip, Outcome, Playback, SpeechGate};

/// Env override naming the sink to play to (node name or serial), for
/// testing and for pinning a device. Unset means PipeWire's default.
const SINK_ENV: &str = "COSMO_SINK";

const BYTES_PER_SAMPLE: usize = std::mem::size_of::<f32>();

enum Cmd {
    Play(Job),
    Stop,
    /// Posted by the stream's `drained` event.
    Drained,
    /// Posted by the stream's `state_changed` event on `Error`.
    StreamFailed(String),
    Shutdown,
}

/// Handle to the playback thread. Cheap to call from any thread; dropping it
/// cancels anything still queued and joins the thread.
pub struct Player {
    tx: pw::channel::Sender<Cmd>,
    gate: SpeechGate,
    thread: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for Player {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Player")
            .field("gate", &self.gate)
            .finish_non_exhaustive()
    }
}

impl Player {
    /// Connect to the session's PipeWire and start the playback thread.
    ///
    /// Fails with [`AudioError::Connect`] when PipeWire is not reachable —
    /// the daemon keeps running without speech and `doctor` says why. No
    /// stream exists until the first clip arrives.
    pub fn start() -> Result<Self, AudioError> {
        let gate = SpeechGate::new();
        let (tx, rx) = pw::channel::channel::<Cmd>();
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(), AudioError>>();

        let thread_gate = gate.clone();
        let self_tx = tx.clone();
        let thread = std::thread::Builder::new()
            .name("cosmo-playback".into())
            .spawn(move || run(rx, self_tx, thread_gate, ready_tx))
            .map_err(|e| AudioError::Connect(format!("spawning the playback thread: {e}")))?;

        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Self {
                tx,
                gate,
                thread: Some(thread),
            }),
            Ok(Err(e)) => {
                let _ = thread.join();
                Err(e)
            }
            Err(_) => {
                let _ = thread.join();
                Err(AudioError::Connect(
                    "playback thread died during setup".into(),
                ))
            }
        }
    }

    /// Queue a clip behind whatever is already playing. Returns at once;
    /// the handle resolves when the audio has drained.
    ///
    /// The daemon's "enqueue speech buffer" API (spec §2.3 DoD): a phrase
    /// cache hit and a freshly synthesized reply both arrive here.
    pub fn play(&self, clip: &Clip) -> Playback {
        let _span = tracing::debug_span!(
            "speak/push",
            rate = clip.sample_rate(),
            samples = clip.samples().len()
        )
        .entered();
        let (job, playback) = Job::new(clip, Instant::now());
        match self.tx.send(Cmd::Play(job)) {
            Ok(()) => playback,
            // The job came back unsent; dropping it closes its handle, but
            // say why rather than a bare `Closed` from a dropped sender.
            Err(_) => Playback::failed(AudioError::Closed),
        }
    }

    /// Cut speech off now: everything playing or queued resolves
    /// [`Outcome::Cancelled`]. Barge-in (phase 5) and `cosmo stop` land here.
    pub fn stop(&self) {
        let _ = self.tx.send(Cmd::Stop);
    }

    /// The half-duplex hook phase 3's capture gates on.
    pub fn gate(&self) -> SpeechGate {
        self.gate.clone()
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        let _ = self.tx.send(Cmd::Shutdown);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// The playback thread body.
fn run(
    rx: pw::channel::Receiver<Cmd>,
    self_tx: pw::channel::Sender<Cmd>,
    gate: SpeechGate,
    ready: mpsc::Sender<Result<(), AudioError>>,
) {
    pw::init();
    let connected = (|| {
        let main_loop = pw::main_loop::MainLoopRc::new(None)?;
        let context = pw::context::ContextRc::new(&main_loop, None)?;
        let core = context.connect_rc(None)?;
        Ok::<_, pw::Error>((main_loop, context, core))
    })();
    let (main_loop, _context, core) = match connected {
        Ok(parts) => parts,
        Err(e) => {
            let _ = ready.send(Err(AudioError::Connect(e.to_string())));
            return;
        }
    };

    let engine = RefCell::new(Engine {
        core,
        main_loop: main_loop.clone(),
        queue: Arc::new(Mutex::new(Queue::with_capacity(64))),
        stream: None,
        backlog: VecDeque::new(),
        gate,
        self_tx,
    });
    let _attached = rx.attach(main_loop.loop_(), move |cmd| {
        engine.borrow_mut().handle(cmd)
    });

    let _ = ready.send(Ok(()));
    main_loop.run();
}

/// The live stream and the listener feeding it. Field order is drop order:
/// the listener is detached before the stream it listens to is destroyed.
struct Active {
    _listener: pw::stream::StreamListener<()>,
    stream: pw::stream::StreamRc,
    rate: u32,
}

/// Main-loop state. Only the RT callback touches `queue` from elsewhere.
struct Engine {
    core: pw::core::CoreRc,
    main_loop: pw::main_loop::MainLoopRc,
    queue: Arc<Mutex<Queue>>,
    stream: Option<Active>,
    /// Jobs not yet admitted to `queue` — waiting on a rate change.
    backlog: VecDeque<Job>,
    gate: SpeechGate,
    self_tx: pw::channel::Sender<Cmd>,
}

impl Engine {
    fn lock(&self) -> MutexGuard<'_, Queue> {
        self.queue.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn handle(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Play(job) => {
                self.gate.set_speaking();
                self.backlog.push_back(job);
                self.pump();
            }
            Cmd::Drained => self.drained(),
            Cmd::Stop => self.finish_all(Ok(Outcome::Cancelled)),
            Cmd::StreamFailed(msg) => {
                tracing::warn!(error = %msg, "playback stream failed; rebuilding on next clip");
                self.finish_all(Err(AudioError::Stream(msg)));
                self.stream = None;
            }
            Cmd::Shutdown => {
                self.finish_all(Ok(Outcome::Cancelled));
                self.stream = None;
                self.main_loop.quit();
            }
        }
    }

    /// Admit backlog jobs that match the stream's rate; rebuild the stream
    /// for a new rate once nothing of the old one is in flight.
    fn pump(&mut self) {
        while let Some(front_rate) = self.backlog.front().map(|j| j.rate) {
            let stream_rate = self.stream.as_ref().map(|a| a.rate);
            if stream_rate == Some(front_rate) {
                let mut queue = self.queue.lock().unwrap_or_else(PoisonError::into_inner);
                while self.backlog.front().is_some_and(|j| j.rate == front_rate) {
                    if let Some(job) = self.backlog.pop_front() {
                        queue.push(job);
                    }
                }
                let draining = queue.draining;
                drop(queue);
                // Mid-drain, the `drained` handler resumes the stream; waking
                // it here would race PipeWire's own drain bookkeeping.
                if !draining && let Some(active) = &self.stream {
                    let _ = active.stream.set_active(true);
                }
                return;
            }
            if !self.lock().is_empty() {
                return; // old rate still playing; `drained` calls back in
            }
            self.stream = None;
            match self.connect(front_rate) {
                Ok(active) => self.stream = Some(active),
                Err(e) => {
                    // Fail this rate's jobs rather than retry forever.
                    tracing::warn!(error = %e, rate = front_rate, "cannot open playback stream");
                    while self.backlog.front().is_some_and(|j| j.rate == front_rate) {
                        if let Some(job) = self.backlog.pop_front() {
                            job.complete(Err(e.clone()));
                        }
                    }
                    if self.backlog.is_empty() {
                        self.gate.set_quiet(Instant::now());
                    }
                }
            }
        }
    }

    fn drained(&mut self) {
        let finished = {
            let mut queue = self.lock();
            queue.draining = false;
            queue.take_finished()
        };
        for job in finished {
            job.complete(Ok(Outcome::Played));
        }
        let more_queued = !self.lock().is_empty();
        if let Some(active) = &self.stream {
            // Clears PipeWire's drained state so `process` is called again
            // (nothing is buffered after a drain, so nothing is discarded).
            let _ = active.stream.flush(false);
            let _ = active.stream.set_active(more_queued);
        }
        if more_queued {
            return;
        }
        if self.backlog.is_empty() {
            self.gate.set_quiet(Instant::now());
        } else {
            self.pump();
        }
    }

    fn finish_all(&mut self, result: Result<Outcome, AudioError>) {
        let in_flight = self.lock().take_all();
        for job in in_flight.into_iter().chain(self.backlog.drain(..)) {
            job.complete(result.clone());
        }
        if let Some(active) = &self.stream {
            let _ = active.stream.flush(false);
            let _ = active.stream.set_active(false);
        }
        self.gate.set_quiet(Instant::now());
    }

    fn connect(&self, rate: u32) -> Result<Active, AudioError> {
        let stream_err = |e: pw::Error| AudioError::Stream(e.to_string());

        let mut props = properties! {
            *pw::keys::MEDIA_TYPE => "Audio",
            *pw::keys::MEDIA_CATEGORY => "Playback",
            // Synthesized speech; closest of PipeWire's standard roles.
            *pw::keys::MEDIA_ROLE => "Accessibility",
            *pw::keys::APP_NAME => "Cosmo",
            *pw::keys::NODE_NAME => "cosmo-speech",
            *pw::keys::NODE_DESCRIPTION => "Cosmo speech",
        };
        if let Ok(target) = std::env::var(SINK_ENV) {
            props.insert(*pw::keys::TARGET_OBJECT, target);
        }
        let stream = pw::stream::StreamRc::new(self.core.clone(), "cosmo-speech", props)
            .map_err(stream_err)?;

        // HALF-DUPLEX (invariant #8): this stream is the only thing cosmo
        // makes audible. `SpeechGate` goes "speaking" when a clip is admitted
        // and quiet on `drained`/stop; phase 3's capture must consult
        // `SpeechGate::mic_open(now, SETTLE)` at the ring buffer rather than
        // inventing a second notion of "cosmo is talking".
        let rt_queue = self.queue.clone();
        let drained_tx = self.self_tx.clone();
        let failed_tx = self.self_tx.clone();
        let listener = stream
            .add_local_listener::<()>()
            .process(move |stream, _| fill(stream, &rt_queue))
            .drained(move |_, _| {
                let _ = drained_tx.send(Cmd::Drained);
            })
            .state_changed(move |_, _, old, new| {
                tracing::debug!(?old, ?new, "playback stream state");
                if let pw::stream::StreamState::Error(msg) = new {
                    let _ = failed_tx.send(Cmd::StreamFailed(msg));
                }
            })
            .register()
            .map_err(stream_err)?;

        let format = format_pod(rate)?;
        let mut params = [Pod::from_bytes(&format)
            .ok_or_else(|| AudioError::Stream("format pod did not parse".into()))?];
        stream
            .connect(
                spa::utils::Direction::Output,
                None,
                pw::stream::StreamFlags::AUTOCONNECT
                    | pw::stream::StreamFlags::MAP_BUFFERS
                    | pw::stream::StreamFlags::RT_PROCESS,
                &mut params,
            )
            .map_err(stream_err)?;
        tracing::debug!(rate, "playback stream connected");

        Ok(Active {
            _listener: listener,
            stream,
            rate,
        })
    }
}

/// RT data-loop callback: copy queued samples into the next buffer.
/// No blocking, no allocation, no frees (see `queue`'s module docs).
fn fill(stream: &pw::stream::Stream, queue: &Mutex<Queue>) {
    let Some(mut buffer) = stream.dequeue_buffer() else {
        return;
    };
    let requested = usize::try_from(buffer.requested()).unwrap_or(usize::MAX);
    let Some(data) = buffer.datas_mut().first_mut() else {
        return;
    };
    let mut drain = false;
    let mut frames = 0;
    if let Some(bytes) = data.data() {
        let capacity = bytes.len() / BYTES_PER_SAMPLE;
        let max = if requested == 0 {
            capacity
        } else {
            requested.min(capacity)
        };
        match queue.try_lock() {
            Ok(mut q) => {
                let mut out = bytes.chunks_exact_mut(BYTES_PER_SAMPLE);
                frames = q.fill(max, Instant::now(), |s| {
                    if let Some(slot) = out.next() {
                        slot.copy_from_slice(&s.to_le_bytes());
                    }
                });
                if frames == 0 && q.exhausted() && !q.draining {
                    q.draining = true;
                    drain = true;
                }
            }
            // The main loop holds the queue (a push or a reap): one quantum
            // of silence is cheaper than blocking the graph.
            Err(_) => {
                bytes[..max * BYTES_PER_SAMPLE].fill(0);
                frames = max;
            }
        }
    }
    let chunk = data.chunk_mut();
    *chunk.offset_mut() = 0;
    *chunk.stride_mut() = BYTES_PER_SAMPLE as i32;
    *chunk.size_mut() = (frames * BYTES_PER_SAMPLE) as u32;
    drop(buffer); // queues it
    if drain {
        let _ = stream.flush(true);
    }
}

/// `EnumFormat` for mono F32LE at `rate`.
fn format_pod(rate: u32) -> Result<Vec<u8>, AudioError> {
    let mut info = spa::param::audio::AudioInfoRaw::new();
    info.set_format(spa::param::audio::AudioFormat::F32LE);
    info.set_rate(rate);
    info.set_channels(1);
    let mut position = [0; spa::param::audio::MAX_CHANNELS];
    position[0] = spa::sys::SPA_AUDIO_CHANNEL_MONO;
    info.set_position(position);

    let object = spa::pod::Object {
        type_: spa::utils::SpaTypes::ObjectParamFormat.as_raw(),
        id: spa::param::ParamType::EnumFormat.as_raw(),
        properties: info.into(),
    };
    spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &spa::pod::Value::Object(object),
    )
    .map(|(cursor, _)| cursor.into_inner())
    .map_err(|e| AudioError::Stream(format!("serialising the format pod: {e:?}")))
}
