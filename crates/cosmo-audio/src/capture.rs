//! The microphone (phase-3 spec §3.2): a native PipeWire capture stream,
//! always on, feeding the pre-roll [`Ring`].
//!
//! Always on, rather than started on key-down: PipeWire takes 50–200 ms to
//! bring a stream up and people start talking before the key registers
//! (blueprint §3.2). The cost is that the mic reads as in use in the panel's
//! audio applet — a real trade, and clipped first words are the worse one.
//!
//! `RT_PROCESS`, for the reason capture is native at all: the callback rides
//! PipeWire's data loop (SCHED_FIFO), so it keeps being serviced while the
//! machine compiles something big. It converts, gates and copies into the
//! ring, and nothing else: no locks, no allocation.
//!
//! **Half-duplex (invariant #8) is enforced here, at the ring.** While
//! [`SpeechGate::mic_open`] says no — cosmo is speaking, or its audio
//! drained less than [`SETTLE`] ago — the callback writes zeros instead of
//! samples. Time keeps flowing (positions stay continuous) but cosmo's own
//! voice never enters the buffer, so it can never be transcribed as a
//! command. This consults the gate playback already drives (§2.3); it does
//! not invent a second notion of "cosmo is talking".

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use pipewire as pw;
use pw::properties::properties;
use pw::spa;
use spa::pod::Pod;

use crate::ring::{CAPTURE_RATE, Ring};
use crate::{AudioError, SETTLE, SpeechGate};

/// Env override naming the source to capture from (node name or serial).
const SOURCE_ENV: &str = "COSMO_SOURCE";

/// How long to wait before reconnecting a stream that went away.
const RECONNECT_AFTER: Duration = Duration::from_secs(2);

/// Counters for `doctor` and the saturated-machine test. Written by the RT
/// callback with relaxed atomics; read whenever.
#[derive(Debug, Default)]
struct Stats {
    callbacks: AtomicU64,
    samples: AtomicU64,
    gated: AtomicU64,
    /// Longest interval between two process callbacks, in microseconds.
    /// At a 16 kHz quantum of a few ms, anything far above the quantum is
    /// the data loop not being serviced — i.e. dropped audio.
    max_gap_us: AtomicU64,
    /// Nanoseconds since `epoch` of the last callback (0 = none yet).
    last_ns: AtomicU64,
    reconnects: AtomicU64,
    streaming: AtomicBool,
}

/// A point-in-time copy of the capture counters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CaptureStats {
    pub callbacks: u64,
    /// Samples received, gated ones included.
    pub samples: u64,
    /// Samples zeroed by the half-duplex gate.
    pub gated: u64,
    pub max_gap: Duration,
    pub reconnects: u64,
    /// The stream is currently in `Streaming` state.
    pub streaming: bool,
}

/// The running capture stream. Dropping it stops the thread.
pub struct Capture {
    ring: Arc<Ring>,
    stats: Arc<Stats>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for Capture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Capture")
            .field("stats", &self.stats())
            .finish_non_exhaustive()
    }
}

impl Capture {
    /// Connect a capture stream and start filling a ring of `ring_seconds`.
    /// Returns once the first connection attempt has been made; a stream
    /// that later errors (a USB mic unplugged) is rebuilt every
    /// [`RECONNECT_AFTER`] until one sticks.
    pub fn start(gate: SpeechGate, ring_seconds: u32) -> Result<Self, AudioError> {
        let ring = Arc::new(Ring::with_seconds(ring_seconds));
        let stats = Arc::new(Stats::default());
        let stop = Arc::new(AtomicBool::new(false));
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(), AudioError>>();

        let (t_ring, t_stats, t_stop) = (ring.clone(), stats.clone(), stop.clone());
        let thread = std::thread::Builder::new()
            .name("cosmo-capture".into())
            .spawn(move || {
                pw::init();
                let mut ready = Some(ready_tx);
                while !t_stop.load(Ordering::Relaxed) {
                    match pump(&t_ring, &t_stats, &t_stop, &gate, &mut ready) {
                        Ok(()) if t_stop.load(Ordering::Relaxed) => break,
                        Ok(()) => tracing::warn!("capture stream ended; reconnecting"),
                        Err(e) => {
                            if let Some(tx) = ready.take() {
                                let _ = tx.send(Err(e));
                                return;
                            }
                            tracing::warn!(error = %e, "capture reconnect failed; retrying");
                        }
                    }
                    t_stats.streaming.store(false, Ordering::Relaxed);
                    t_stats.reconnects.fetch_add(1, Ordering::Relaxed);
                    std::thread::sleep(RECONNECT_AFTER);
                }
            })
            .map_err(|e| AudioError::Connect(format!("spawning the capture thread: {e}")))?;

        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Self {
                ring,
                stats,
                stop,
                thread: Some(thread),
            }),
            Ok(Err(e)) => {
                let _ = thread.join();
                Err(e)
            }
            Err(_) => {
                let _ = thread.join();
                Err(AudioError::Connect(
                    "capture thread died during setup".into(),
                ))
            }
        }
    }

    /// The ring the stream fills. Readers take windows of it by position.
    pub fn ring(&self) -> &Arc<Ring> {
        &self.ring
    }

    pub fn stats(&self) -> CaptureStats {
        let s = &self.stats;
        CaptureStats {
            callbacks: s.callbacks.load(Ordering::Relaxed),
            samples: s.samples.load(Ordering::Relaxed),
            gated: s.gated.load(Ordering::Relaxed),
            max_gap: Duration::from_micros(s.max_gap_us.load(Ordering::Relaxed)),
            reconnects: s.reconnects.load(Ordering::Relaxed),
            streaming: s.streaming.load(Ordering::Relaxed),
        }
    }

    /// Forget the longest gap seen so far (start a fresh measurement).
    pub fn reset_max_gap(&self) {
        self.stats.max_gap_us.store(0, Ordering::Relaxed);
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// One connection: owns the loop, context and stream until the stream
/// errors or `stop` is set. `Err` only when setup itself failed.
fn pump(
    ring: &Arc<Ring>,
    stats: &Arc<Stats>,
    stop: &Arc<AtomicBool>,
    gate: &SpeechGate,
    ready: &mut Option<mpsc::Sender<Result<(), AudioError>>>,
) -> Result<(), AudioError> {
    let connect = |e: pw::Error| AudioError::Connect(e.to_string());
    let stream_err = |e: pw::Error| AudioError::Stream(e.to_string());

    let main_loop = pw::main_loop::MainLoopRc::new(None).map_err(connect)?;
    let context = pw::context::ContextRc::new(&main_loop, None).map_err(connect)?;
    let core = context.connect_rc(None).map_err(connect)?;

    let mut props = properties! {
        *pw::keys::MEDIA_TYPE => "Audio",
        *pw::keys::MEDIA_CATEGORY => "Capture",
        *pw::keys::MEDIA_ROLE => "Communication",
        *pw::keys::APP_NAME => "Cosmo",
        *pw::keys::NODE_NAME => "cosmo-mic",
        *pw::keys::NODE_DESCRIPTION => "Cosmo microphone",
    };
    if let Ok(target) = std::env::var(SOURCE_ENV) {
        props.insert(*pw::keys::TARGET_OBJECT, target);
    }
    let stream = pw::stream::StreamRc::new(core.clone(), "cosmo-mic", props).map_err(stream_err)?;

    let epoch = Instant::now();
    let (rt_ring, rt_stats, rt_gate) = (ring.clone(), stats.clone(), gate.clone());
    let state_loop = main_loop.clone();
    let state_stats = stats.clone();
    let _listener = stream
        .add_local_listener::<()>()
        .state_changed(move |_, _, old, new| {
            tracing::debug!(?old, ?new, "capture stream state");
            state_stats.streaming.store(
                matches!(new, pw::stream::StreamState::Streaming),
                Ordering::Relaxed,
            );
            if matches!(new, pw::stream::StreamState::Error(_)) {
                state_loop.quit(); // the outer loop reconnects
            }
        })
        .process(move |stream, _| on_process(stream, &rt_ring, &rt_stats, &rt_gate, epoch))
        .register()
        .map_err(stream_err)?;

    let format = format_pod()?;
    let mut params = [Pod::from_bytes(&format)
        .ok_or_else(|| AudioError::Stream("format pod did not parse".into()))?];
    stream
        .connect(
            spa::utils::Direction::Input,
            None,
            pw::stream::StreamFlags::AUTOCONNECT
                | pw::stream::StreamFlags::MAP_BUFFERS
                | pw::stream::StreamFlags::RT_PROCESS,
            &mut params,
        )
        .map_err(stream_err)?;

    // Poll the stop flag from the loop itself; the loop owns everything.
    let stop_loop = main_loop.clone();
    let stop_flag = stop.clone();
    let timer = main_loop.loop_().add_timer(move |_| {
        if stop_flag.load(Ordering::Relaxed) {
            stop_loop.quit();
        }
    });
    let tick = Some(Duration::from_millis(100));
    let _ = timer.update_timer(tick, tick);

    if let Some(tx) = ready.take() {
        let _ = tx.send(Ok(()));
    }
    main_loop.run();
    Ok(())
}

/// RT data loop: convert, gate, copy. No locks, no allocation.
fn on_process(
    stream: &pw::stream::Stream,
    ring: &Ring,
    stats: &Stats,
    gate: &SpeechGate,
    epoch: Instant,
) {
    let now = Instant::now();
    let now_ns = u64::try_from(now.duration_since(epoch).as_nanos())
        .unwrap_or(u64::MAX)
        .max(1);
    let last = stats.last_ns.swap(now_ns, Ordering::Relaxed);
    if last != 0 {
        stats
            .max_gap_us
            .fetch_max(now_ns.saturating_sub(last) / 1_000, Ordering::Relaxed);
    }
    stats.callbacks.fetch_add(1, Ordering::Relaxed);

    let Some(mut buffer) = stream.dequeue_buffer() else {
        return;
    };
    let Some(data) = buffer.datas_mut().first_mut() else {
        return;
    };
    let len = data.chunk().size() as usize;
    let Some(bytes) = data.data() else {
        return;
    };
    let bytes = &bytes[..len.min(bytes.len())];
    let n = bytes.len() / 4;
    stats.samples.fetch_add(n as u64, Ordering::Relaxed);

    // HALF-DUPLEX: cosmo's own voice never enters the ring (invariant #8).
    if !gate.mic_open(now, SETTLE) {
        ring.write_silence(n);
        stats.gated.fetch_add(n as u64, Ordering::Relaxed);
        return;
    }
    let mut window = [0f32; 256];
    for block in bytes.chunks(4 * window.len()) {
        let m = block.len() / 4;
        for (slot, b) in window.iter_mut().zip(block.chunks_exact(4)) {
            *slot = f32::from_le_bytes([b[0], b[1], b[2], b[3]]);
        }
        ring.write(&window[..m]);
    }
}

/// `EnumFormat`: mono F32LE at [`CAPTURE_RATE`]; PipeWire converts.
fn format_pod() -> Result<Vec<u8>, AudioError> {
    let mut info = spa::param::audio::AudioInfoRaw::new();
    info.set_format(spa::param::audio::AudioFormat::F32LE);
    info.set_rate(CAPTURE_RATE);
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
