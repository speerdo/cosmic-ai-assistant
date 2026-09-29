//! Ears (phase-3 spec §3.7): the trigger key, the microphone ring and the
//! speech models, turned into `Listening` and transcripts.
//!
//! One recording at a time. Key down (or `cosmo listen`) interrupts any
//! reply being spoken, marks the ring 750 ms back, and moves to
//! `Listening`; partials stream as `Event::Transcript { final: false }`.
//! Key up ends it; the committed text arrives as one
//! `Event::Transcript { final: true }`, and the state returns to `Idle`.
//! **Transcripts are shown, not acted on**: wiring them into turns is phase
//! 4/5, with the spoken-confirm path closed first (spec, "Not in phase 3").
//!
//! The controller ([`run`]) reads a [`Ring`] and a channel of [`Trigger`]s,
//! not the devices, so a test can drive it with a WAV and synthetic key
//! edges. The daemon wires the real capture stream and hotkey watcher to
//! it in [`start`].

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cosmo_audio::Ring;
use cosmo_ipc::{Event, State};
use cosmo_stt::{Stt, SttConfig};
use tokio::sync::mpsc;
use tracing::Instrument;

/// Audio kept from before the key registered, so the first syllable
/// survives key latency (the ring holds far more).
pub const PREROLL_MS: u32 = 750;
/// Longest wait after a release for a word still being finished. Cut short
/// the moment the VAD hears silence, so a speaker who stopped before
/// letting go waits for nothing. On the user's recordings real speech ran
/// at most ~100 ms past the release (phase-3 findings §6f, less the VAD's
/// debounce); 300 ms cost the slowest commands most of their budget
/// (phase-4 findings §1d).
pub const MAX_TAIL: Duration = Duration::from_millis(200);
/// Holds shorter than this are taps or Right Ctrl shortcuts, not
/// utterances (findings §3a): discarded, never transcribed.
pub const MIN_HOLD: Duration = Duration::from_millis(300);
/// How often the ring is drained into the session.
const POLL: Duration = Duration::from_millis(30);
/// Hard cap on one recording, beyond the silence backstop.
const MAX_RECORDING: Duration = Duration::from_secs(60);

/// What starts and stops a recording.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    /// The trigger key went down, at this instant.
    Press(Instant),
    /// The trigger key came up.
    Release(Instant),
    /// `cosmo listen`: start if idle, stop if listening.
    Toggle,
}

/// What the controller needs from the rest of the daemon.
pub trait Host: Send + Sync {
    fn set_state(&self, state: State);
    fn emit(&self, event: Event);
    /// Cut off a reply being spoken: the user is talking now.
    fn interrupt_speech(&self);
    /// Not accepting input (`cosmo toggle`).
    fn paused(&self) -> bool;
    /// Hotwords for this recording, one per line (see
    /// `cosmo_stt::hotwords::Hotwords::for_app`).
    fn hotwords(&self) -> String;
    /// A committed transcript becomes a turn (phase-4 spec §4.2). Runs in
    /// the background: the controller goes straight back to listening.
    /// `source` says whether the key was held, which decides whether the
    /// words may confirm a held action (gate invariant #5).
    fn turn(self: Arc<Self>, text: String, source: cosmo_gate::UtteranceSource);
}

/// The speech models, which take a couple of seconds to load after start.
#[derive(Clone, Default)]
pub struct Models(Arc<Mutex<ModelsState>>);

#[derive(Clone, Default)]
enum ModelsState {
    #[default]
    Loading,
    Ready(Stt),
    Failed(String),
}

impl Models {
    pub fn ready(stt: Stt) -> Self {
        Self(Arc::new(Mutex::new(ModelsState::Ready(stt))))
    }

    fn set(&self, state: ModelsState) {
        *self.0.lock().unwrap() = state;
    }

    fn stt(&self) -> Result<Stt, String> {
        match &*self.0.lock().unwrap() {
            ModelsState::Ready(stt) => Ok(stt.clone()),
            ModelsState::Loading => Err("speech models are still loading".into()),
            ModelsState::Failed(e) => Err(e.clone()),
        }
    }

    /// (ok, detail) for `doctor`.
    pub fn doctor(&self) -> (bool, String) {
        match &*self.0.lock().unwrap() {
            ModelsState::Loading => (false, "loading…".into()),
            ModelsState::Failed(e) => (false, e.clone()),
            ModelsState::Ready(stt) => {
                let describe = |role: &str, m: &Option<cosmo_stt::Loaded>| match m {
                    Some(m) => format!(
                        "{role} {} ({:.1}s{})",
                        m.name,
                        m.load.as_secs_f64(),
                        if m.hotwords { ", hotwords" } else { "" }
                    ),
                    None => format!("{role} off"),
                };
                (
                    true,
                    format!(
                        "{}; {}",
                        describe("streaming", &stt.streaming_model),
                        describe("offline", &stt.offline_model)
                    ),
                )
            }
        }
    }
}

/// The daemon's handle: triggers in, status out.
#[derive(Clone)]
pub struct Ears {
    triggers: mpsc::UnboundedSender<Trigger>,
    listening: Arc<AtomicBool>,
    models: Models,
    devices: Arc<dyn Fn() -> (Option<cosmo_audio::CaptureStats>, usize) + Send + Sync>,
}

impl Ears {
    /// `cosmo listen`. Returns whether a recording will be running.
    pub fn toggle(&self) -> bool {
        let _ = self.triggers.send(Trigger::Toggle);
        !self.listening.load(Ordering::Acquire)
    }

    pub fn models(&self) -> &Models {
        &self.models
    }

    /// Capture stream stats and the number of keyboards carrying the
    /// trigger, for `doctor`.
    pub fn devices(&self) -> (Option<cosmo_audio::CaptureStats>, usize) {
        (self.devices)()
    }
}

/// Wire the real devices: capture (gated by playback's `gate`), the hotkey
/// watcher on `trigger_key`, and the models, loading in the background.
/// Nothing here is fatal: whatever fails shows up in `doctor`.
pub fn start(
    host: Arc<dyn Host>,
    gate: cosmo_audio::SpeechGate,
    cfg: &cosmo_config::Config,
) -> Result<Ears, String> {
    let capture = cosmo_audio::Capture::start(gate, 30).map_err(|e| e.to_string())?;
    let ring = Arc::clone(capture.ring());
    let (tx, rx) = mpsc::unbounded_channel();
    let edges = tx.clone();
    let watcher = cosmo_hotkey::Watcher::start(cfg.trigger_key, move |ev| {
        let _ = edges.send(match ev.edge {
            cosmo_hotkey::Edge::Pressed => Trigger::Press(ev.at),
            cosmo_hotkey::Edge::Released => Trigger::Release(ev.at),
        });
    });
    let watcher = match watcher {
        Ok(w) => Some(w),
        Err(e) => {
            tracing::warn!(error = %e, "hotkey unavailable; `cosmo listen` still works");
            None
        }
    };
    let models = Models::default();
    let loading = models.clone();
    let stt_config = SttConfig::from_config(cfg);
    std::thread::Builder::new()
        .name("cosmo-asr-load".into())
        .spawn(move || {
            let result = stt_config
                .ok_or_else(|| "no cache directory for the models".to_owned())
                .and_then(|c| Stt::load(&c).map_err(|e| e.to_string()));
            match result {
                Ok(stt) => loading.set(ModelsState::Ready(stt)),
                Err(e) => {
                    tracing::warn!(error = %e, "speech recognition unavailable");
                    loading.set(ModelsState::Failed(e));
                }
            }
        })
        .map_err(|e| e.to_string())?;

    let listening = Arc::new(AtomicBool::new(false));
    tokio::spawn(run(ring, models.clone(), rx, host, Arc::clone(&listening)));
    let capture = Arc::new(capture);
    let watcher = Arc::new(watcher);
    Ok(Ears {
        triggers: tx,
        listening,
        models,
        devices: Arc::new(move || {
            (
                Some(capture.stats()),
                watcher.as_ref().as_ref().map_or(0, |w| w.devices().len()),
            )
        }),
    })
}

/// How a recording ended.
enum End {
    /// Key up (or a second `cosmo listen`), at this instant.
    Released(Instant),
    /// The silence backstop or the length cap: the release may be lost.
    Timeout,
}

/// The controller: one recording at a time, until `triggers` closes.
pub async fn run(
    ring: Arc<Ring>,
    models: Models,
    mut triggers: mpsc::UnboundedReceiver<Trigger>,
    host: Arc<dyn Host>,
    listening: Arc<AtomicBool>,
) {
    while let Some(trigger) = triggers.recv().await {
        let (pressed, by_key) = match trigger {
            Trigger::Press(at) => (at, true),
            Trigger::Toggle => (Instant::now(), false),
            // A release with no recording: its press was discarded or
            // came before startup.
            Trigger::Release(_) => continue,
        };
        if host.paused() {
            continue;
        }
        let stt = match models.stt() {
            Ok(stt) => stt,
            Err(why) => {
                host.emit(Event::Log {
                    line: format!("not listening: {why}"),
                });
                continue;
            }
        };
        listening.store(true, Ordering::Release);
        let committed = record(&ring, &stt, &mut triggers, host.as_ref(), pressed, by_key).await;
        listening.store(false, Ordering::Release);
        host.set_state(State::Idle);
        if let Some((text, latency)) = committed {
            host.emit(Event::Transcript {
                text: text.clone(),
                r#final: true,
                latency_ms: Some(latency.as_millis() as u64),
            });
            if !text.trim().is_empty() {
                // Only a physical key hold may confirm; `cosmo listen` is
                // an open mic (anything could have run it).
                let source = if by_key {
                    cosmo_gate::UtteranceSource::KeyHeld
                } else {
                    cosmo_gate::UtteranceSource::OpenMic
                };
                Arc::clone(&host).turn(text, source);
            }
        }
    }
}

/// One recording. Returns the committed text and release → commit time,
/// or `None` for a discarded tap.
async fn record(
    ring: &Ring,
    stt: &Stt,
    triggers: &mut mpsc::UnboundedReceiver<Trigger>,
    host: &dyn Host,
    pressed: Instant,
    by_key: bool,
) -> Option<(String, Duration)> {
    host.interrupt_speech();
    // The press was read a moment ago; reach back from *then*.
    let late = u32::try_from(pressed.elapsed().as_millis()).unwrap_or(0);
    let mut pos = ring.mark_preroll(PREROLL_MS + late);
    let mut session = match stt.session(&host.hotwords()) {
        Ok(s) => s,
        Err(e) => {
            host.emit(Event::Log {
                line: format!("not listening: {e}"),
            });
            return None;
        }
    };
    host.set_state(State::Listening);

    let started = Instant::now();
    let mut tick = tokio::time::interval(POLL);
    let drain = |session: &mut cosmo_stt::Session, pos: &mut u64| -> bool {
        let (from, samples) = ring.read(*pos, ring.now());
        if from > *pos {
            tracing::warn!(lost = from - *pos, "recording fell behind the ring");
        }
        *pos = from + samples.len() as u64;
        let mut backstop = false;
        for e in session.push(&samples) {
            match e {
                cosmo_stt::Event::Partial(text) => host.emit(Event::Transcript {
                    text,
                    r#final: false,
                    latency_ms: None,
                }),
                cosmo_stt::Event::Backstop => backstop = true,
                cosmo_stt::Event::Segment { .. } => {}
            }
        }
        backstop
    };

    let end = loop {
        tokio::select! {
            _ = tick.tick() => {
                if drain(&mut session, &mut pos) || started.elapsed() > MAX_RECORDING {
                    break End::Timeout;
                }
            }
            trigger = triggers.recv() => match trigger {
                Some(Trigger::Release(at)) if by_key => break End::Released(at),
                Some(Trigger::Toggle) => break End::Released(Instant::now()),
                // A press while recording by `cosmo listen`, or a stray
                // release: the recording continues.
                Some(_) => {}
                None => break End::Timeout,
            },
        }
    };

    let released = match end {
        End::Released(at) => {
            if by_key && at.duration_since(pressed) < MIN_HOLD {
                session.cancel();
                return None;
            }
            // Wait out the tail only while speech is still going.
            let deadline = at + MAX_TAIL;
            drain(&mut session, &mut pos);
            while session.hearing_speech() && Instant::now() < deadline {
                tokio::time::sleep(POLL.min(deadline.saturating_duration_since(Instant::now())))
                    .await;
                drain(&mut session, &mut pos);
            }
            tracing::debug!(tail_ms = at.elapsed().as_millis() as u64, "release tail");
            at
        }
        End::Timeout => {
            drain(&mut session, &mut pos);
            Instant::now()
        }
    };

    let span = tracing::info_span!("transcript");
    match session.finish().instrument(span).await {
        Ok(t) => {
            let latency = released.elapsed();
            tracing::info!(
                latency_ms = latency.as_millis() as u64,
                decode_ms = t.latency.as_millis() as u64,
                segments = t.segments.len(),
                "transcript committed"
            );
            Some((t.text, latency))
        }
        Err(e) => {
            host.emit(Event::Log {
                line: format!("transcription failed: {e}"),
            });
            None
        }
    }
}
