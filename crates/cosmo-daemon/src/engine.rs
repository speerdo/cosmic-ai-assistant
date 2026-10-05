//! The engine: daemon state machine + turn handling.
//!
//! Phase 1 scope: `Say` runs the reasoning loop once `cosmo-reason` exists;
//! until then the engine executes locally-recognisable turns and holds
//! everything the gate parks, so the IPC surface and the CLI are real.
//!
//! ## Span names (plan §1.1 — agreed now, parsed by scripts/bench-*)
//!
//! One span per hop, named exactly:
//! - `turn` — the whole user turn
//! - `transcript` — (phase 3) audio → committed transcript
//! - `gate` — every gate verdict
//! - `tool` — every tool execution (`tool` field carries the name)
//! - `reason` — every model round trip
//! - `ack` — (phase 4) reflex dispatch → ack started
//! - `speak/synthesize`, `speak/push`, `speak/first_audio` — (phase 2) the
//!   spoken reply's hops (`speech.rs`, `cosmo-audio`)

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use tokio::sync::broadcast;
use tracing::Instrument;

use cosmo_config::Config;
use cosmo_gate::{ConfirmResult, Gate, LockSource, LockState, UtteranceSource};
use cosmo_mcp::McpHost;
use cosmo_reason::secret::ProviderKey;
use cosmo_reason::tools::ToolHost;

use crate::speech::{DefaultSpeechKey, Speech, SpeechSink, StateCell};
use crate::toolhost::DaemonToolHost;

/// How `announce` reaches the user (spec §2.7): spoken in the active voice
/// when speech is live, else — or if speaking fails — a desktop
/// notification, else only the log line the announcer always writes.
struct AnnounceDelivery {
    speech: Option<Arc<Speech>>,
}

impl cosmo_tools::announce::Delivery for AnnounceDelivery {
    fn deliver<'a>(
        &'a self,
        text: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
        Box::pin(async move {
            if let Some(speech) = &self.speech {
                match speech.announce(text).await {
                    Ok(()) => return,
                    Err(e) => tracing::warn!(error = %e, "announce not spoken; notifying instead"),
                }
            }
            if let Err(e) = notify(text).await {
                tracing::warn!(error = %e, "announce notification failed");
            }
        })
    }
}

/// `org.freedesktop.Notifications.Notify` on the session bus.
async fn notify(body: &str) -> zbus::Result<()> {
    let conn = zbus::Connection::session().await?;
    let hints: std::collections::HashMap<&str, zbus::zvariant::Value<'_>> =
        std::collections::HashMap::new();
    conn.call_method(
        Some("org.freedesktop.Notifications"),
        "/org/freedesktop/Notifications",
        Some("org.freedesktop.Notifications"),
        "Notify",
        &(
            "Cosmo",
            0u32,
            "",
            "Cosmo",
            body,
            Vec::<&str>::new(),
            hints,
            -1i32,
        ),
    )
    .await?;
    Ok(())
}

/// Lock policy mode (findings §L).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LockMode {
    /// logind `LockedHint` is authoritative (GNOME sets it; COSMIC today
    /// does not, so this mode must never be selected on COSMIC).
    LogindHint,
    /// COSMIC: deny lock-sensitive tools outright until a verified source
    /// exists. Fail-closed, not a bug.
    CosmicDenyAll,
}

/// Where to start the retained tail of `history` so the result is a *valid*
/// conversation, keeping at most `keep` messages.
///
/// A `role: "tool"` message is only meaningful as the answer to an assistant
/// message that announced that `tool_call` id. Cutting the window so it opens
/// on one leaves an orphan the chat-completions API rejects — so the cut
/// point walks forward past any leading tool results. Trimming a few extra
/// messages is free; poisoning every later turn is not.
fn tail_start(history: &[serde_json::Value], keep: usize) -> usize {
    let mut start = history.len().saturating_sub(keep);
    while start < history.len() && history[start]["role"] == "tool" {
        start += 1;
    }
    start
}

/// logind `LockedHint` probe over the system bus.
struct LogindLock {
    conn: zbus::Connection,
    session_path: String,
}

impl LogindLock {
    async fn connect() -> anyhow::Result<Self> {
        let conn = zbus::Connection::system().await?;
        // $XDG_SESSION_ID, or (as a systemd user service, which lacks it)
        // logind's display session for this user.
        let session_path = crate::lock::session_path(&conn).await?;
        Ok(Self { conn, session_path })
    }
}

impl LockSource for LogindLock {
    fn probe(&self) -> LockState {
        let locked = zbus::block_on(async {
            use zbus::fdo::PropertiesProxy;
            let iface = PropertiesProxy::builder(&self.conn)
                .destination("org.freedesktop.login1")?
                .path(self.session_path.clone())?
                .interface("org.freedesktop.login1.Session")?
                .build()
                .await?;
            let value: zbus::zvariant::OwnedValue = iface
                .get("org.freedesktop.login1.Session".try_into()?, "LockedHint")
                .await?;
            Ok::<_, zbus::Error>(bool::try_from(&value)?)
        });
        match locked {
            Ok(true) => LockState::Locked,
            Ok(false) => LockState::Unlocked,
            Err(e) => {
                tracing::warn!(error = %e, "LockedHint probe failed; failing closed");
                LockState::Unknown
            }
        }
    }
}
use cosmo_ipc::{
    Command, ConfirmOutcome as IpcConfirmOutcome, DoctorCheck, DoctorReport, Event, PendingHold,
    Response, State, StatusInfo, TurnResult,
};

/// Whether a local server is up and has `model`; the reason, with its fix,
/// when not.
async fn local_ready(url: &str, model: &str) -> Result<(), String> {
    match cosmo_reason::provider::local_models(url).await {
        Err(why) => Err(format!(
            "{why}: start a local model server (Ollama: ollama.com/download, then \
             `ollama serve`), or point api_base at yours"
        )),
        Ok(models) if !models.iter().any(|m| m == model) => Err(format!(
            "the local server doesn't have {model}: `ollama pull {model}`, or pick one it has \
             ({})",
            if models.is_empty() {
                "none yet".to_owned()
            } else {
                models.join(", ")
            }
        )),
        Ok(_) => Ok(()),
    }
}

pub struct Engine {
    cfg: Config,
    gate: Arc<Gate>,
    events: broadcast::Sender<Event>,
    state: Arc<StateCell>,
    paused: AtomicBool,
    version: &'static str,
    /// Lock policy mode from findings §L: `Logind` on GNOME (hint flips),
    /// `DenyAll` on COSMIC (logind stays `no` even when locked — upstream
    /// greeter never sets it — so sensitive tools are denied outright and
    /// the logind probe is not trusted).
    lock_mode: LockMode,
    logind: Option<LogindLock>,
    /// Combined agent + native tool host (None until the agent connects).
    tools: Mutex<Option<Arc<dyn ToolHost>>>,
    /// How many agent tools that host carries, for `doctor`.
    agent_tools: std::sync::atomic::AtomicUsize,
    /// Conversation history for the reasoning loop (compacted per turn by
    /// keeping only the tail; phase 5 replaces with the Realtime session).
    history: Mutex<Vec<serde_json::Value>>,
    /// Spoken replies; `None` when there is no audio sink (built without
    /// the `speech` feature, or PipeWire unreachable at startup).
    speech: Option<Arc<Speech>>,
    /// Why `speech` is `None`, for `doctor`.
    speech_absent: String,
    /// Hotword biasing for transcripts: installed app names today; phase 4
    /// adds the reflex phrases and per-app sets.
    #[cfg(feature = "ears")]
    hotwords: cosmo_stt::hotwords::Hotwords,
    /// Microphone, trigger key and speech models (phase 3). Set once, after
    /// the engine is shared, since the controller holds the engine.
    #[cfg(feature = "ears")]
    ears: OnceLock<crate::ears::Ears>,
    /// Why there are no ears, for `doctor` and `cosmo listen`.
    ears_absent: OnceLock<String>,
    /// The reflex path (phase 4): safe verbs, matched and carried out
    /// locally. Set once, after start-up; until then turns go to reasoning.
    reflex: OnceLock<crate::reflex::Reflex>,
    /// Numbers reflex tool calls for their events.
    reflex_calls: std::sync::atomic::AtomicU64,
    /// "I need an API key" has been said this run (it's said once).
    no_key_said: AtomicBool,
    /// The reasoning client, created on the first reasoning turn (the key
    /// is resolved then, not at start-up) and kept: its HTTP connection
    /// stays warm instead of a TLS handshake per turn (phase-5 spec §5.2).
    /// Async lock: held across the turn, so reasoning turns take turns.
    reasoner: tokio::sync::Mutex<Option<cosmo_reason::Reasoner>>,
    /// The turn in progress failed for want of a key. Set by the reasoning
    /// path, read and cleared by `utterance` (turns needing reasoning run
    /// one at a time: ears commits one recording at a time).
    no_key_hit: AtomicBool,
    /// The last reasoning turn's usage, for `doctor`.
    last_usage: Mutex<Option<cosmo_reason::TurnUsage>>,
    /// The reasoning settings (provider, model, api_base, api_format):
    /// switchable while running (`Command::ReasoningSet`), unlike the rest
    /// of `cfg`. Everything reasoning reads them from here.
    reasoning_cfg: Mutex<Config>,
    /// A browser sign-in waiting for its browser; a new one replaces it.
    sign_in: Mutex<Option<tokio::task::AbortHandle>>,
    /// COSMIC's lock state, tracked live (`crate::lock`) once started. It
    /// sets the gate itself; without it COSMIC stays deny-all.
    lock_tracker: OnceLock<Arc<Mutex<crate::lock::Tracker>>>,
}

impl Engine {
    pub async fn new(cfg: Config, events: broadcast::Sender<Event>) -> Self {
        let logind = LogindLock::connect().await.ok();
        let lock_mode = match std::env::var("XDG_CURRENT_DESKTOP").as_deref() {
            Ok(d) if d.eq_ignore_ascii_case("COSMIC") => LockMode::CosmicDenyAll,
            _ => LockMode::LogindHint,
        };
        Self {
            reasoning_cfg: Mutex::new(cfg.clone()),
            sign_in: Mutex::new(None),
            lock_tracker: OnceLock::new(),
            cfg,
            gate: Arc::new(Gate::new()),
            state: Arc::new(StateCell::new(events.clone())),
            events,
            paused: AtomicBool::new(false),
            version: env!("CARGO_PKG_VERSION"),
            lock_mode,
            logind,
            tools: Mutex::new(None),
            agent_tools: std::sync::atomic::AtomicUsize::new(0),
            history: Mutex::new(Vec::new()),
            speech: None,
            speech_absent: "built without the `speech` feature".into(),
            #[cfg(feature = "ears")]
            hotwords: cosmo_stt::hotwords::curated(&cosmo_stt::hotwords::desktop_apps()),
            #[cfg(feature = "ears")]
            ears: OnceLock::new(),
            ears_absent: OnceLock::new(),
            reflex: OnceLock::new(),
            reflex_calls: std::sync::atomic::AtomicU64::new(0),
            no_key_said: AtomicBool::new(false),
            reasoner: tokio::sync::Mutex::new(None),
            no_key_hit: AtomicBool::new(false),
            last_usage: Mutex::new(None),
        }
    }

    /// Give the engine its ears (`crate::ears::start`).
    #[cfg(feature = "ears")]
    pub fn attach_ears(&self, ears: crate::ears::Ears) {
        let _ = self.ears.set(ears);
    }

    /// Give the engine its reflex path.
    pub fn attach_reflex(&self, reflex: crate::reflex::Reflex) {
        let _ = self.reflex.set(reflex);
    }

    /// The reflex path for one turn: `Some` if it handled the turn, `None`
    /// to escalate to reasoning, which happens when nothing matched well
    /// enough, or a match's action failed (blueprint §2's escalation rule:
    /// escalate rather than report the failure).
    async fn try_reflex(&self, text: &str) -> Option<TurnResult> {
        let reflex = self.reflex.get()?;
        let matched = reflex.matcher.match_text(text)?;
        if matched.confidence < cosmo_reflex::THRESHOLD {
            tracing::debug!(confidence = matched.confidence, intent = ?matched.intent, "reflex: below threshold, escalating");
            return None;
        }
        let intent = matched.intent;
        let (tool, args) = intent.tool_call();
        // Defence in depth: reflex verbs are Allow by construction (the
        // gate test in cosmo-reflex), and are checked again here anyway.
        let annotations = cosmo_gate::Annotations {
            read_only: cosmo_reflex::Intent::READ_ONLY,
            destructive: cosmo_reflex::Intent::DESTRUCTIVE,
        };
        if self.gate.verdict_for_call(tool, &args, &annotations) != cosmo_gate::Verdict::Allow {
            tracing::error!(tool, "reflex: gate did not allow a reflex verb; escalating");
            return None;
        }
        let action = intent.describe();
        let call_id = format!(
            "reflex-{}",
            self.reflex_calls.fetch_add(1, Ordering::Relaxed) + 1
        );
        let _ = self.events.send(Event::ToolStarted {
            call_id: call_id.clone(),
            tool: tool.to_owned(),
            args: args.to_string(),
        });
        self.set_state(State::Acting);
        let started = std::time::Instant::now();
        let outcome = reflex
            .actuator
            .act(&intent)
            .instrument(tracing::info_span!("ack", tool))
            .await;
        let latency_ms = started.elapsed().as_millis() as u64;
        let _ = self.events.send(Event::ToolFinished {
            call_id,
            tool: tool.to_owned(),
            ok: outcome.is_ok(),
            latency_ms,
            summary: match &outcome {
                Ok(s) | Err(s) => s.clone(),
            },
        });
        match outcome {
            Ok(summary) => {
                if let (Some(speech), Some(ack)) = (&self.speech, intent.ack()) {
                    speech.play_phrase(ack);
                }
                tracing::info!(%action, latency_ms, "reflex");
                self.set_state(State::Idle);
                Some(TurnResult::Reflexed { action, summary })
            }
            Err(why) => {
                tracing::info!(%action, %why, "reflex action failed; escalating");
                None
            }
        }
    }

    /// Record why there are no ears.
    pub fn ears_unavailable(&self, reason: String) {
        let _ = self.ears_absent.set(reason);
    }

    fn ears_absent(&self) -> String {
        self.ears_absent
            .get()
            .cloned()
            .unwrap_or_else(|| "built without the `ears` feature".into())
    }

    fn listen(&self) -> Response {
        #[cfg(feature = "ears")]
        if let Some(ears) = self.ears.get() {
            if self.paused.load(Ordering::SeqCst) {
                return Response::Error {
                    message: "daemon is paused (cosmo toggle to resume)".into(),
                };
            }
            if let Err(message) = ears.models().doctor().0.then_some(()).ok_or_else(|| {
                format!("speech recognition not ready: {}", ears.models().doctor().1)
            }) {
                return Response::Error { message };
            }
            return Response::Listening {
                active: ears.toggle(),
            };
        }
        Response::Error {
            message: format!("not listening — {}", self.ears_absent()),
        }
    }

    fn ears_check(&self) -> DoctorCheck {
        #[cfg(feature = "ears")]
        if let Some(ears) = self.ears.get() {
            let (models_ok, models) = ears.models().doctor();
            let (capture, keyboards) = ears.devices();
            let capture_ok = capture.as_ref().is_some_and(|c| c.streaming);
            let capture = match capture {
                Some(c) if c.streaming => format!(
                    "mic streaming (longest callback gap {} ms, {} reconnects)",
                    c.max_gap.as_millis(),
                    c.reconnects
                ),
                _ => "mic not streaming — is a PipeWire source available?".into(),
            };
            let key = if keyboards == 0 {
                format!(
                    "trigger key {}: no readable keyboard has it — the \
                     /dev/input/event* nodes need the logind uaccess ACL \
                     (a local seat session); `cosmo listen` still works",
                    self.cfg.trigger_key
                )
            } else {
                format!(
                    "trigger key {} on {keyboards} keyboard{}",
                    self.cfg.trigger_key,
                    if keyboards == 1 { "" } else { "s" }
                )
            };
            let wake = match ears.wake() {
                None => "; wake word off (wake_word: true to use it)".to_owned(),
                Some(w) => format!(
                    "; wake word \"{}\" on: {} checks, {} wakes{}",
                    w.phrase,
                    w.stats.checks.load(Ordering::Relaxed),
                    w.stats.wakes.load(Ordering::Relaxed),
                    w.stats
                        .last
                        .lock()
                        .unwrap()
                        .as_ref()
                        .map(|t| format!(", last heard \"{t}\""))
                        .unwrap_or_default()
                ),
            };
            let barge = if self.cfg.barge_in {
                "; ⚠ barge_in is on: the mic stays open while cosmo speaks, so on \
                 speakers it will hear itself (use a headset, or set barge_in: false)"
            } else {
                ""
            };
            return DoctorCheck {
                name: "ears".into(),
                ok: models_ok && capture_ok && keyboards > 0,
                warn: self.cfg.barge_in,
                detail: format!("{capture}; {key}; {models}{wake}{barge}"),
            };
        }
        DoctorCheck {
            name: "ears".into(),
            ok: false,
            warn: false,
            detail: format!("not listening — {}", self.ears_absent()),
        }
    }

    /// Give the engine an audio sink: replies are spoken from now on.
    pub fn attach_speech(&mut self, sink: Arc<dyn SpeechSink>) {
        self.speech = Some(Arc::new(Speech::new(
            self.cfg.clone(),
            sink,
            Arc::new(DefaultSpeechKey),
            Arc::clone(&self.state),
        )));
    }

    /// Load the voice provider in the background (see [`Speech::warm`]).
    pub fn warm_speech(&self) {
        if let Some(speech) = &self.speech {
            speech.warm();
        }
    }

    /// Record why no sink could be attached (shown by `doctor`).
    pub fn speech_unavailable(&mut self, reason: String) {
        self.speech = None;
        self.speech_absent = reason;
    }

    fn no_speech(&self) -> Response {
        Response::Error {
            message: format!("speech is off — {}", self.speech_absent),
        }
    }

    async fn voice_list(&self, provider: Option<&str>) -> Response {
        let Some(speech) = &self.speech else {
            return self.no_speech();
        };
        match speech.list_voices(provider).await {
            Ok((provider, active, voices)) => Response::Voices {
                provider,
                active,
                voices: voices
                    .into_iter()
                    .map(|v| cosmo_ipc::VoiceInfo {
                        id: v.id,
                        label: v.label,
                        accent: v.accent.as_str().to_owned(),
                        gender: v.gender.map(|g| format!("{g:?}").to_lowercase()),
                    })
                    .collect(),
            },
            Err(message) => Response::Error { message },
        }
    }

    /// Cut off any reply still being spoken: a new turn has begun.
    fn interrupt_speech(&self) {
        if let Some(speech) = &self.speech {
            speech.interrupt();
        }
    }

    /// Connect the MCP agent and register the combined tool host. Called at
    /// startup; a failure is not fatal — doctor reports it and `say` errors
    /// per turn (graceful absence, plan §1.3).
    pub async fn connect_tools(&self) -> anyhow::Result<()> {
        let cfg = Arc::new(self.cfg.clone());
        let host = McpHost::connect(cfg)
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        let tmux_session = cosmo_config::load()?.tmux_session;
        // The reflex path's desktop actions, as tools reasoning can call
        // too: "open a browser on workspace 2 and…" needs them in sequence.
        let desktop = self.reflex.get().map(|r| crate::toolhost::DesktopTools {
            matcher: Arc::clone(&r.matcher),
            actuator: Arc::clone(&r.actuator),
        });
        let host = DaemonToolHost::new(
            Arc::new(host),
            &tmux_session,
            cosmo_tools::announce::Announcer::with_delivery(Arc::new(AnnounceDelivery {
                speech: self.speech.clone(),
            })),
            desktop,
        );
        let count = host.agent_tool_count();
        self.attach_tools(Arc::new(host), count);
        Ok(())
    }

    /// Register the tool host the reasoning path uses. `connect_tools` does
    /// this with the MCP agent; tests pass a fake.
    pub fn attach_tools(&self, host: Arc<dyn ToolHost>, agent_tool_count: usize) {
        self.agent_tools.store(agent_tool_count, Ordering::SeqCst);
        *self.tools.lock().unwrap() = Some(host);
    }

    /// On COSMIC, track the lock from logind's signals and the compositor
    /// (`crate::lock`), so screen tools work while unlocked. If that can't
    /// start, COSMIC stays deny-all.
    pub async fn track_cosmic_lock(&self) {
        if self.lock_mode != LockMode::CosmicDenyAll {
            return;
        }
        match crate::lock::start(Arc::clone(&self.gate)).await {
            Ok(tracker) => {
                let _ = self.lock_tracker.set(tracker);
                tracing::info!("COSMIC lock state tracked (logind Lock + activated window)");
            }
            Err(e) => {
                tracing::warn!(error = %e, "can't track COSMIC's lock; screen tools stay denied")
            }
        }
    }

    /// Refresh the gate's lock state according to the active policy and
    /// log (once per call) the decision. Called at startup and before each
    /// turn.
    pub fn refresh_lock(&self) {
        let state = match self.lock_mode {
            // Findings §L: no unprivileged lock source on COSMIC today —
            // sensitive tools are denied outright (gate denies `Unknown`).
            // Tracked: the tracker keeps the gate current by itself.
            LockMode::CosmicDenyAll if self.lock_tracker.get().is_some() => self.gate.lock_state(),
            LockMode::CosmicDenyAll => {
                self.gate.set_lock_state(LockState::Unknown);
                LockState::Unknown
            }
            LockMode::LogindHint => match self.logind.as_ref() {
                Some(src) => self.gate.refresh_lock(src),
                None => {
                    self.gate.set_lock_state(LockState::Unknown);
                    LockState::Unknown
                }
            },
        };
        tracing::debug!(?state, mode = ?self.lock_mode, "lock state refreshed");
    }

    /// The policy gate this engine runs every tool call through.
    ///
    /// Exposed so the hold/confirm path can be driven end to end in tests
    /// without an API key: a hold is normally parked by the reasoning loop,
    /// which needs a model. The bug this enabled catching — `cosmo confirm`
    /// never resolving anything — lived through a green gate suite precisely
    /// because no test drove `Engine::handle` itself.
    pub fn gate(&self) -> &Arc<Gate> {
        &self.gate
    }

    pub fn set_state(&self, state: State) {
        self.state.set(state);
    }

    fn state(&self) -> State {
        self.state.get()
    }

    /// Remove a held action without running it, and say so. True when it
    /// was pending.
    fn drop_hold(&self, token: &str, why: &str) -> bool {
        let ok = self.gate.reject(token);
        if ok {
            let _ = self.events.send(Event::HoldResolved {
                token: token.to_owned(),
                executed: false,
                summary: why.into(),
            });
            // Nothing left to wait on: leave Waiting, or the next turn
            // starts from a state that is no longer true.
            if self.gate.pending().is_empty() && self.state() == State::Waiting {
                self.set_state(State::Idle);
            }
        }
        ok
    }

    /// Cancel held actions older than `max_age`: a voice request nobody
    /// confirmed within minutes is stale, and the overlay shouldn't wait
    /// on it forever. Expiring never runs anything. Returns how many.
    pub fn expire_holds(&self, max_age: std::time::Duration) -> usize {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let stale: Vec<String> = self
            .gate
            .pending()
            .into_iter()
            .filter(|p| now.saturating_sub(p.parked_at_ms) > max_age.as_millis() as u64)
            .map(|p| p.token)
            .collect();
        let n = stale
            .iter()
            .filter(|t| self.drop_hold(t, "expired: not confirmed in time"))
            .count();
        if n > 0 {
            tracing::info!(expired = n, "unconfirmed held actions expired");
        }
        n
    }

    /// Handle one command. This is the single entry point from the socket.
    pub async fn handle(&self, cmd: Command) -> Response {
        tracing::debug!(?cmd, "command");
        match cmd {
            Command::Status => Response::Status(self.status()),
            Command::Doctor => Response::Doctor(self.doctor().await),
            Command::Say { text } => self.say(text).await,
            Command::Confirm { token } => self.confirm(token).await,
            Command::Cancel { token } => {
                let ok = self.drop_hold(&token, "rejected");
                Response::Cancelled {
                    ok,
                    reason: (!ok).then(|| "no pending hold with that token".to_string()),
                }
            }
            Command::VoiceList { provider } => self.voice_list(provider.as_deref()).await,
            Command::VoicePreview { provider, voice } => {
                let Some(speech) = &self.speech else {
                    return self.no_speech();
                };
                match speech.preview(provider.as_deref(), &voice).await {
                    Ok(key) => Response::VoicePreviewed {
                        provider: key.provider,
                        voice: key.voice,
                    },
                    Err(message) => Response::Error { message },
                }
            }
            Command::VoiceSet { provider, voice } => {
                let Some(speech) = &self.speech else {
                    return self.no_speech();
                };
                let path = cosmo_config::config_path();
                match speech.set_voice(provider.as_deref(), &voice, &path).await {
                    Ok(key) => Response::VoiceSet {
                        provider: key.provider,
                        voice: key.voice,
                        persisted_to: path.display().to_string(),
                    },
                    Err(message) => Response::Error { message },
                }
            }
            Command::Reasoning => Response::Reasoning(self.reasoning_info().await),
            Command::ReasoningSet { provider, model } => {
                self.set_reasoning(&provider, &model).await
            }
            Command::SignInStart { provider } => self.sign_in_start(&provider).await,
            Command::StoreKey { provider, key } => {
                if cosmo_reason::provider::preset(&provider).is_none() {
                    return Response::Error {
                        message: format!("unknown provider {provider:?}"),
                    };
                }
                match cosmo_reason::secret::store_key(&provider, key.0.trim()).await {
                    Ok(()) => {
                        self.key_changed(&provider).await;
                        Response::KeyStored { provider }
                    }
                    Err(e) => Response::Error {
                        message: format!("storing the key failed: {e}"),
                    },
                }
            }
            Command::Listen => self.listen(),
            Command::Toggle => {
                let paused = !self.paused.load(Ordering::SeqCst);
                self.paused.store(paused, Ordering::SeqCst);
                Response::Toggled { paused }
            }
        }
    }

    /// Every provider, whether it's connected, and what's in use.
    async fn reasoning_info(&self) -> cosmo_ipc::ReasoningInfo {
        use cosmo_ipc::{Connect, ProviderInfo};
        use cosmo_reason::provider::{Endpoint, PRESETS, local_models, runs_remotely};
        let rcfg = self.reasoning_cfg.lock().unwrap().clone();
        let model = Endpoint::resolve(&rcfg, None)
            .map(|e| e.model)
            .unwrap_or_default();
        let probes = PRESETS.iter().map(|p| {
            let rcfg = &rcfg;
            async move {
                let (connected, models, why) = if p.needs_key {
                    let stored = tokio::time::timeout(
                        std::time::Duration::from_secs(2),
                        cosmo_reason::secret::resolve_keyring(p.name),
                    )
                    .await;
                    match stored {
                        Ok(Ok(_)) => (true, Vec::new(), None),
                        Ok(Err(e)) => (false, Vec::new(), Some(e.to_string())),
                        Err(_) => (false, Vec::new(), Some("the keyring didn't answer".into())),
                    }
                } else {
                    // The local server this config would use.
                    let url = if rcfg.provider == p.name && !rcfg.api_base.is_empty() {
                        rcfg.api_base.clone()
                    } else {
                        p.chat_url.unwrap_or_default().to_owned()
                    };
                    match local_models(&url).await {
                        Ok(m) => (true, m, None),
                        Err(why) => (false, Vec::new(), Some(why)),
                    }
                };
                ProviderInfo {
                    name: p.name.into(),
                    label: p.label.into(),
                    connect: match (p.needs_key, p.browser_login) {
                        (false, _) => Connect::Local,
                        (true, Some(_)) => Connect::Browser,
                        (true, None) => Connect::Key,
                    },
                    connected,
                    key_page: p.key_page.into(),
                    default_model: p.default_model.into(),
                    remote_models: models
                        .iter()
                        .filter(|m| runs_remotely(m))
                        .cloned()
                        .collect(),
                    models: models.into_iter().filter(|m| !runs_remotely(m)).collect(),
                    note: why.or(p.note.map(str::to_owned)),
                }
            }
        });
        cosmo_ipc::ReasoningInfo {
            active: rcfg.provider.clone(),
            model,
            providers: futures::future::join_all(probes).await,
        }
    }

    /// Reason with `provider` (and `model`, empty = its default) from the
    /// next turn, and persist it.
    async fn set_reasoning(&self, provider: &str, model: &str) -> Response {
        if cosmo_reason::provider::preset(provider).is_none() {
            return Response::Error {
                message: format!(
                    "unknown provider {provider:?} (known: {})",
                    cosmo_reason::provider::names()
                ),
            };
        }
        let path = cosmo_config::config_path();
        let same = self.reasoning_cfg.lock().unwrap().provider == provider;
        let mut fields = vec![("provider", provider), ("model", model)];
        // Another provider: an endpoint or wire set for the old one would
        // send this one's requests to the wrong place.
        if !same {
            fields.extend([("api_base", ""), ("api_format", "")]);
        }
        match cosmo_config::set_string_fields(&path, &fields) {
            Ok(cfg) => {
                *self.reasoning_cfg.lock().unwrap() = cfg;
                self.key_changed(provider).await;
                // The conversation so far belongs to the old model.
                if !same {
                    self.history.lock().unwrap().clear();
                }
                tracing::info!(provider, model, "reasoning switched");
                let model = cosmo_reason::provider::Endpoint::resolve(
                    &self.reasoning_cfg.lock().unwrap(),
                    None,
                )
                .map(|e| e.model)
                .unwrap_or_default();
                Response::ReasoningSet {
                    provider: provider.to_owned(),
                    model,
                }
            }
            Err(e) => Response::Error {
                message: format!("couldn't update {}: {e}", path.display()),
            },
        }
    }

    /// A key was stored, or the provider changed: the next turn builds a
    /// fresh client (and says "no key" again if there still isn't one).
    async fn key_changed(&self, provider: &str) {
        if self.reasoning_cfg.lock().unwrap().provider == provider {
            *self.reasoner.lock().await = None;
            self.no_key_said.store(false, Ordering::SeqCst);
        }
    }

    /// Start a browser sign-in; the URL goes back to the asking client to
    /// open (only it should: there's an applet per monitor), and the
    /// outcome arrives as `Event::SignIn`.
    async fn sign_in_start(&self, provider: &str) -> Response {
        let Some(login) = cosmo_reason::provider::preset(provider).and_then(|p| p.browser_login)
        else {
            return Response::Error {
                message: format!("{provider} has no browser sign-in; paste an API key instead"),
            };
        };
        let (url, pending) = match cosmo_reason::login::start(&login).await {
            Ok(started) => started,
            Err(e) => {
                return Response::Error {
                    message: e.to_string(),
                };
            }
        };
        let events = self.events.clone();
        let name = provider.to_owned();
        let task = tokio::spawn(async move {
            let (ok, detail) = match pending.finish().await {
                Ok(key) => match cosmo_reason::secret::store_key(&name, &key).await {
                    Ok(()) => (true, format!("signed in to {name}")),
                    Err(e) => (false, format!("signed in, but storing the key failed: {e}")),
                },
                Err(e) => (false, e.to_string()),
            };
            tracing::info!(provider = %name, ok, "browser sign-in finished");
            let _ = events.send(Event::SignIn {
                provider: name,
                ok,
                detail,
            });
        });
        if let Some(old) = self.sign_in.lock().unwrap().replace(task.abort_handle()) {
            old.abort();
        }
        Response::SignInUrl {
            provider: provider.to_owned(),
            url,
        }
    }

    fn status(&self) -> StatusInfo {
        StatusInfo {
            state: self.state(),
            paused: self.paused.load(Ordering::SeqCst),
            version: self.version.to_owned(),
            pending_holds: self
                .gate
                .pending()
                .into_iter()
                .map(|p| PendingHold {
                    token: p.token,
                    action: p.description,
                    parked_at_ms: p.parked_at_ms,
                })
                .collect(),
        }
    }

    /// The last reasoning turn's token use and the server's rate limit
    /// (invariant #7), so they're visible without watching events.
    fn usage_check(&self) -> DoctorCheck {
        let detail = match &*self.last_usage.lock().unwrap() {
            None => "no reasoning turn yet (reflex commands use none)".to_owned(),
            Some(u) => {
                let mut d = format!(
                    "last turn: {} tokens ({} in, {} out)",
                    u.total_tokens, u.prompt_tokens, u.completion_tokens
                );
                match (u.remaining_requests, u.remaining_tokens) {
                    (None, None) => d.push_str("; the provider reports no rate limit"),
                    (r, t) => {
                        d.push_str("; remaining:");
                        if let Some(r) = r {
                            d.push_str(&format!(" {r} requests"));
                        }
                        if let Some(t) = t {
                            d.push_str(&format!(" {t} tokens"));
                        }
                    }
                }
                d
            }
        };
        DoctorCheck {
            name: "token use".into(),
            ok: true,
            warn: false,
            detail,
        }
    }

    async fn doctor(&self) -> DoctorReport {
        let cfg_ok = cosmo_config::load().is_ok();
        let socket_ok = std::path::Path::new(&cosmo_ipc::socket_path()).exists();
        let tools = self.tools.lock().unwrap().clone();
        let (agent_ok, agent_detail) = match tools.as_ref() {
            Some(_) => {
                let n = self.agent_tools.load(Ordering::SeqCst);
                (true, format!("agent connected, {n} tools allowlisted"))
            }
            None => (
                false,
                "agent not connected — is computer-use-linux installed? \
                 (`npm install -g @agent-sh/computer-use-linux`)"
                    .to_owned(),
            ),
        };
        let tracked = self.lock_tracker.get();
        let lock_warn = self.lock_mode == LockMode::CosmicDenyAll
            && tracked.is_none_or(|_| self.gate.lock_state() != LockState::Unlocked);
        let (lock_ok, lock_detail) = match (self.lock_mode, self.gate.lock_state()) {
            (LockMode::CosmicDenyAll, _) if tracked.is_some() => (
                true,
                format!(
                    "COSMIC, tracked from logind's Lock signal and the active window: {}",
                    tracked.expect("checked").lock().unwrap().describe()
                ),
            ),
            (LockMode::LogindHint, LockState::Unlocked) => (
                true,
                "logind LockedHint says unlocked — screenshot/click/type available".to_owned(),
            ),
            (LockMode::LogindHint, LockState::Locked) => (
                true,
                "session locked — sensitive tools refuse until unlock".to_owned(),
            ),
            (LockMode::LogindHint, LockState::Unknown) => (
                false,
                "logind probe failed — sensitive tools refuse (fail-closed)".to_owned(),
            ),
            // Fail-closed by design until upstream sets LockedHint: a
            // warning, so a working install can read green.
            (LockMode::CosmicDenyAll, _) => (
                true,
                "no lock-state source on COSMIC (findings §L) — screenshot / \
                 click / type / clipboard denied outright until the upstream \
                 greeter sets LockedHint. run_in_terminal is unaffected: it \
                 is governed by the command matcher, not by lock state"
                    .to_owned(),
            ),
        };
        let (speech_ok, speech_detail) = match &self.speech {
            Some(speech) => speech.doctor(),
            None => (
                false,
                format!("replies are text only — {}", self.speech_absent),
            ),
        };
        // Key presence: the doctor's third distinct state set (plan §1.4).
        let rcfg = self.reasoning_cfg.lock().unwrap().clone();
        let provider = &rcfg.provider;
        let needs_key = cosmo_reason::provider::preset(provider).is_none_or(|p| p.needs_key);
        let env_key = ["COSMO_API_KEY"]
            .into_iter()
            .chain((provider == "openai").then_some("OPENAI_API_KEY"))
            .any(|v| std::env::var(v).is_ok_and(|k| !k.trim().is_empty()));
        // A reasoner exists only once its key resolved. Busy (mid-turn)
        // means it exists too.
        let key_loaded = self
            .reasoner
            .try_lock()
            .map(|slot| slot.is_some())
            .unwrap_or(true);
        let key_detail = if !needs_key {
            (
                true,
                "none needed: the model runs on this computer".to_owned(),
            )
        } else if env_key {
            (true, "key present (source: env — dev/CI only)".to_owned())
        } else if key_loaded {
            (
                true,
                format!("the {provider} key is loaded (source: keyring)"),
            )
        } else {
            // Not loaded yet (it's read at the first turn): ask the keyring
            // whether one is there, so a stored key isn't reported missing.
            // The key itself is dropped at once.
            use cosmo_reason::secret::{ReasonKind, resolve_keyring};
            let probe =
                tokio::time::timeout(std::time::Duration::from_secs(3), resolve_keyring(provider))
                    .await;
            match probe {
                Ok(Ok(_)) => (
                    true,
                    format!("the {provider} key is stored in the keyring (read at the first turn)"),
                ),
                Ok(Err(ReasonKind::Missing)) => (
                    false,
                    match cosmo_reason::provider::preset(provider).and_then(|p| p.browser_login) {
                        Some(_) => format!(
                            "not connected to {provider} — run `cosmo auth-login` \
                             to sign in with your browser"
                        ),
                        None => format!(
                            "no {provider} key stored — run `cosmo auth-login` to paste one, \
                             or `cosmo auth-login --provider openrouter` to sign in with \
                             your browser instead"
                        ),
                    },
                ),
                Ok(Err(ReasonKind::KeyringLocked)) => (
                    false,
                    "the keyring is locked — unlock it (log in again, or open Passwords and Keys)"
                        .to_owned(),
                ),
                Ok(Err(e)) => (false, format!("keyring unavailable — {e}")),
                Err(_) => (false, "the keyring didn't answer within 3 s".to_owned()),
            }
        };
        // Which service reasoning goes to, and as what.
        let env_base = std::env::var("COSMO_API_BASE").ok();
        let reasoning_detail =
            match cosmo_reason::provider::Endpoint::resolve(&rcfg, env_base.as_deref()) {
                Ok(e) => {
                    let format = match e.format {
                        cosmo_reason::provider::Format::OpenAiChat => "chat completions",
                        cosmo_reason::provider::Format::AnthropicMessages => "Messages API",
                    };
                    let note = e
                        .provider
                        .note
                        .map(|n| format!(" — note: {n}"))
                        .unwrap_or_default();
                    (
                        true,
                        format!(
                            "{} · {} · {format} at {}{note}",
                            e.provider.name, e.model, e.url
                        ),
                    )
                }
                Err(err) => (false, format!("{err} — fix `provider` in config.ron")),
            };
        // A local model: is the server up, and is the model on it?
        let reasoning_detail =
            match cosmo_reason::provider::Endpoint::resolve(&rcfg, env_base.as_deref()) {
                Ok(e) if !needs_key => match local_ready(&e.url, &e.model).await {
                    Ok(()) if cosmo_reason::provider::runs_remotely(&e.model) => (
                        true,
                        format!(
                            "⚠ {} is one of Ollama's cloud models: it runs on ollama.com, not \
                             this computer, so what you say leaves the machine. Pick a model \
                             without :cloud to keep it local",
                            e.model
                        ),
                    ),
                    Ok(()) => (
                        true,
                        format!("{} on the local server at {}", e.model, e.url),
                    ),
                    Err(why) => (false, why),
                },
                _ => reasoning_detail,
            };
        DoctorReport {
            checks: vec![
                DoctorCheck {
                    name: "config".into(),
                    ok: cfg_ok,
                    warn: false,
                    detail: if cfg_ok {
                        "config.ron loads".into()
                    } else {
                        "config.ron unreadable — delete it to regenerate defaults".into()
                    },
                },
                DoctorCheck {
                    name: "control socket".into(),
                    ok: socket_ok,
                    warn: false,
                    detail: "the daemon is listening (you are talking to it)".into(),
                },
                DoctorCheck {
                    name: "lock policy".into(),
                    ok: lock_ok,
                    warn: lock_warn,
                    detail: lock_detail,
                },
                DoctorCheck {
                    name: "api key".into(),
                    ok: key_detail.0,
                    warn: false,
                    detail: key_detail.1,
                },
                DoctorCheck {
                    name: "reasoning".into(),
                    ok: reasoning_detail.0,
                    warn: false,
                    detail: reasoning_detail.1,
                },
                DoctorCheck {
                    name: "agent (MCP)".into(),
                    ok: agent_ok,
                    warn: false,
                    detail: agent_detail,
                },
                DoctorCheck {
                    name: "speech".into(),
                    ok: speech_ok,
                    warn: false,
                    detail: speech_detail,
                },
                self.ears_check(),
                self.usage_check(),
            ],
        }
    }

    /// A typed turn (`cosmo say`).
    async fn say(&self, text: String) -> Response {
        Response::Said {
            result: self.utterance(text, UtteranceSource::Typed).await,
        }
    }

    /// One user turn, typed or spoken; `source` is where it came from.
    /// Whole-utterance confirms resolve locally (and only from a source
    /// that may confirm: gate invariant #5); everything else runs the
    /// reasoning tool loop.
    pub async fn utterance(&self, text: String, source: UtteranceSource) -> TurnResult {
        let result = self.turn_inner(text, source).await;
        // A typed turn's failure is printed by the CLI. A spoken one has no
        // terminal waiting on it, so it is answered aloud (spec §4.5).
        let no_key = self.no_key_hit.swap(false, Ordering::SeqCst);
        if source != UtteranceSource::Typed && matches!(result, TurnResult::Failed { .. }) {
            self.say_failure(no_key);
        }
        result
    }

    /// The spoken answer to a failed spoken turn: why, the first time it's
    /// a missing API key; a short "can't" otherwise.
    fn say_failure(&self, no_key: bool) {
        let Some(speech) = &self.speech else { return };
        if no_key && !self.no_key_said.swap(true, Ordering::SeqCst) {
            speech.play_phrase("err-no-key");
        } else {
            speech.play_phrase("err-cant");
        }
    }

    async fn turn_inner(&self, text: String, source: UtteranceSource) -> TurnResult {
        if self.paused.load(Ordering::SeqCst) {
            return cosmo_ipc::TurnResult::Failed {
                reason: "daemon is paused (cosmo toggle to resume)".into(),
            };
        }

        let turn_span = tracing::info_span!("turn");
        self.interrupt_speech();
        self.gate.begin_turn();
        // Refresh lock state before every turn (plan §1.2: between turns
        // and on lock-source signals).
        self.refresh_lock();

        // Whole-utterance confirm: resolve locally, no model round trip.
        let confirm = self.gate.confirm_utterance(&text, source);
        if let ConfirmResult::NeedsKey = confirm {
            // Heard on an open mic: it confirms nothing, and it must not
            // reach the model dressed as the user's approval either.
            if let Some(speech) = &self.speech {
                speech.play_phrase("confirm-needs-key");
            }
            return cosmo_ipc::TurnResult::ConfirmNeedsKey;
        }
        if let ConfirmResult::Executed(parked) = confirm {
            self.set_state(State::Acting);
            let summary = self.execute_parked(&parked.tool, &parked.args).await;
            let _ = self.events.send(Event::HoldResolved {
                token: parked.token.clone(),
                executed: true,
                summary: summary.clone(),
            });
            self.set_state(State::Idle);
            return cosmo_ipc::TurnResult::ConfirmedLocally {
                token: parked.token,
                summary,
            };
        }

        // Reflex path: a safe verb, done locally, no model.
        if let Some(result) = self.try_reflex(&text).await {
            return result;
        }

        // Reasoning path.
        let Some(tools) = self.tools.lock().unwrap().clone() else {
            return cosmo_ipc::TurnResult::Failed {
                reason: "agent not connected — check `cosmo doctor`".into(),
            };
        };

        // Key resolution is lazy (first reasoning turn) — a locked keyring
        // at boot must not have killed the daemon (plan §1.4). A failure
        // isn't kept: the next turn tries again.
        let mut slot = self.reasoner.lock().await;
        if slot.is_none() {
            let rcfg = self.reasoning_cfg.lock().unwrap().clone();
            let key = ProviderKey::reasoning(&rcfg.provider);
            match cosmo_reason::Reasoner::new(Arc::new(rcfg), &key) {
                Ok(r) => *slot = Some(r),
                Err(cosmo_reason::ReasonError::NoKey(msg)) => {
                    self.no_key_hit.store(true, Ordering::SeqCst);
                    return cosmo_ipc::TurnResult::Failed { reason: msg };
                }
                Err(e) => {
                    return cosmo_ipc::TurnResult::Failed {
                        reason: e.to_string(),
                    };
                }
            }
        }
        let reasoner = slot.as_mut().expect("just created");
        // What `remember` holds, fresh each turn: a note added last turn
        // applies to this one (phase-5 spec §5.7). No file is no notes.
        let memory = tokio::fs::read_to_string(cosmo_tools::memory::memory_path())
            .await
            .unwrap_or_default();
        // The profile too, fresh each turn (setup may have just run).
        let profile = cosmo_config::profile::load()
            .map(|p| p.prompt_line())
            .unwrap_or_default();
        // The date: "how long ago…" and "is it out yet?" need it, and a
        // model's own sense of it is its training cut-off.
        let context = format!("Today is {}. {profile}", cosmo_tools::search::today());
        reasoner.set_context(context.trim(), &memory);

        self.set_state(State::Thinking);
        let mut history = self.history.lock().unwrap().clone();
        // The reply is spoken as it streams, sentence by sentence (§5.3).
        let spoken = self.speech.as_ref().map(|s| s.speak_stream());
        let on_text = |t: &str| {
            if let Some(stream) = &spoken {
                stream.push(t);
            }
        };
        let outcome = reasoner
            .turn_streaming(&text, &self.gate, tools.as_ref(), &mut history, &on_text)
            .instrument(turn_span)
            .await;
        let usage = reasoner.last_usage().clone();
        drop(slot);
        if usage.requests > 0 {
            *self.last_usage.lock().unwrap() = Some(usage.clone());
            let _ = self.events.send(Event::Usage {
                prompt_tokens: usage.prompt_tokens,
                completion_tokens: usage.completion_tokens,
                total_tokens: usage.total_tokens,
                remaining_requests: usage.remaining_requests,
                remaining_tokens: usage.remaining_tokens,
            });
        }
        // Whatever the outcome, the stream ends here: text already written
        // (a reply, or words before a held call) is spoken in full.
        let streamed = spoken.is_some();
        if let Some(stream) = spoken {
            stream.finish();
        }
        // Persist a bounded tail of the history (phase 5 replaces this).
        {
            let mut hist = self.history.lock().unwrap();
            *hist = history;
            let tail_from = tail_start(&hist, 20);
            hist.drain(0..tail_from);
        }

        match outcome {
            Ok(cosmo_reason::ToolOutcome::Reply(reply)) => {
                let _ = self.events.send(Event::Reply {
                    text: reply.clone(),
                });
                // Already being spoken as it streamed; that task moves
                // Thinking → Speaking → Idle. The CLI gets its text now.
                if !streamed {
                    self.set_state(State::Idle);
                }
                cosmo_ipc::TurnResult::Completed {
                    reply,
                    held: self.pending_ipc_holds(),
                }
            }
            Ok(cosmo_reason::ToolOutcome::Held { token, tool }) => {
                self.set_state(State::Waiting);
                // The phrase cache's first live consumer: an instant spoken
                // cue that something is waiting on the user. The token
                // itself is never spoken; confirmation stays local.
                if let Some(speech) = &self.speech {
                    speech.play_phrase("confirm-hold");
                }
                let action = format!("{tool} — confirm with: cosmo confirm {token}");
                // The event carries the action alone; clients add their own
                // confirm affordance (the CLI prints the command, the
                // overlay will draw a button).
                let _ = self.events.send(Event::Held {
                    token: token.clone(),
                    action: tool.clone(),
                });
                cosmo_ipc::TurnResult::Completed {
                    reply: action,
                    held: self.pending_ipc_holds(),
                }
            }
            Ok(cosmo_reason::ToolOutcome::ToolRan { .. }) => {
                self.set_state(State::Idle);
                cosmo_ipc::TurnResult::Completed {
                    reply: "tool ran".into(),
                    held: self.pending_ipc_holds(),
                }
            }
            Err(cosmo_reason::ReasonError::NoKey(msg)) => {
                self.no_key_hit.store(true, Ordering::SeqCst);
                self.set_state(State::Idle);
                cosmo_ipc::TurnResult::Failed { reason: msg }
            }
            Err(e) => {
                self.set_state(State::Idle);
                cosmo_ipc::TurnResult::Failed {
                    reason: e.to_string(),
                }
            }
        }
    }

    fn pending_ipc_holds(&self) -> Vec<PendingHold> {
        self.gate
            .pending()
            .into_iter()
            .map(|p| PendingHold {
                token: p.token,
                action: p.description,
                parked_at_ms: p.parked_at_ms,
            })
            .collect()
    }

    /// `cosmo confirm <token>` — executes the parked call locally (invariant
    /// #4: never a model round trip).
    ///
    /// The `begin_turn()` is load-bearing, not bookkeeping. Invariant #2 says
    /// a confirmation only takes effect after a *genuinely new user turn*,
    /// and the gate enforces it by refusing to resolve a hold parked in the
    /// current turn. A `cosmo confirm <token>` is exactly such a new turn: a
    /// separate, deliberate act by the user, out of band from the model, with
    /// the token in hand. Without this line the counter still reads the turn
    /// the hold was parked in, `confirm_token` returns `Unknown`, and **no
    /// CLI confirmation can ever succeed** — which is what DoD §1.5's
    /// `say "shut the machine down"` → `cosmo confirm` path did.
    ///
    /// What invariant #2 actually forbids — a model approving its own gated
    /// call inside one response — is unaffected: that path is
    /// [`Gate::same_response_verdict`], which escalates to Deny before
    /// anything is ever parked.
    async fn confirm(&self, token: String) -> Response {
        self.interrupt_speech();
        self.gate.begin_turn();
        let result = self.gate.confirm_token(&token);
        match result {
            ConfirmResult::Executed(parked) => {
                self.set_state(State::Acting);
                let summary = self.execute_parked(&parked.tool, &parked.args).await;
                let _ = self.events.send(Event::HoldResolved {
                    token: parked.token.clone(),
                    executed: true,
                    summary: summary.clone(),
                });
                self.set_state(State::Idle);
                Response::Confirm {
                    outcome: IpcConfirmOutcome::Executed {
                        token: parked.token,
                        summary,
                    },
                }
            }
            ConfirmResult::Unknown | ConfirmResult::NonePending => Response::Confirm {
                outcome: IpcConfirmOutcome::Unknown,
            },
            // Neither comes from a token confirm; answered as unknown.
            ConfirmResult::NotAConfirm | ConfirmResult::NeedsKey => Response::Confirm {
                outcome: IpcConfirmOutcome::Unknown,
            },
        }
    }

    /// Execute a parked (confirmed) call through the same tool host the loop
    /// uses. Sensitive tools still respect the lock policy: the gate denied
    /// them at park time, so anything parked here passed Surface A/B.
    async fn execute_parked(&self, tool: &str, args: &serde_json::Value) -> String {
        let Some(tools) = self.tools.lock().unwrap().clone() else {
            return "agent not connected".into();
        };
        tools
            .execute(tool, args.clone())
            .instrument(tracing::info_span!("tool", tool = %tool, confirmed = true))
            .await
    }
}

#[cfg(feature = "ears")]
impl crate::ears::Host for Engine {
    fn set_state(&self, state: State) {
        self.state.set(state);
    }

    fn emit(&self, event: Event) {
        let _ = self.events.send(event);
    }

    fn interrupt_speech(&self) {
        Engine::interrupt_speech(self);
    }

    fn paused(&self) -> bool {
        self.paused.load(Ordering::SeqCst)
    }

    fn hotwords(&self) -> String {
        // The focus lookup is a Wayland round trip: only paid for when a
        // per-app set exists (none until phase 4).
        let app = self
            .hotwords
            .has_app_sets()
            .then(|| cosmo_focus::FocusMirror::connect().ok()?.focused_app_id())
            .flatten();
        self.hotwords.for_app(app.as_deref())
    }

    fn turn(self: Arc<Self>, text: String, source: UtteranceSource) {
        tokio::spawn(async move {
            let result = self.utterance(text, source).await;
            tracing::info!(?source, ?result, "spoken turn");
        });
    }
}

#[cfg(test)]
mod tests {
    use super::tail_start;
    use serde_json::json;

    fn assistant_with_call(id: &str) -> serde_json::Value {
        json!({"role": "assistant", "tool_calls": [{"id": id}]})
    }
    fn tool_result(id: &str) -> serde_json::Value {
        json!({"role": "tool", "tool_call_id": id, "content": "ok"})
    }

    /// Truncating the retained history must never open the window on a
    /// `role: "tool"` message: it answers an assistant `tool_call` that the
    /// cut just discarded, and the API rejects the orphan — turning history
    /// compaction into a delayed, hard-to-attribute 400.
    #[test]
    fn tail_never_starts_on_an_orphan_tool_result() {
        let history = vec![
            json!({"role": "user", "content": "one"}),
            assistant_with_call("a"),
            tool_result("a"),
            tool_result("a2"),
            json!({"role": "assistant", "content": "done"}),
        ];
        // Keeping 3 would cut at index 2, a tool result. Walk forward past
        // every leading tool result instead.
        let start = tail_start(&history, 3);
        assert_eq!(start, 4);
        assert_eq!(history[start]["role"], "assistant");

        // A cut that already lands on a valid boundary is left alone.
        assert_eq!(tail_start(&history, 4), 1);
        // Keeping more than there is keeps everything.
        assert_eq!(tail_start(&history, 99), 0);
    }

    /// All-tool-results is degenerate but must not index out of bounds.
    #[test]
    fn tail_start_handles_all_tool_results() {
        let history = vec![tool_result("a"), tool_result("b")];
        assert_eq!(tail_start(&history, 1), history.len());
        assert_eq!(tail_start(&[], 5), 0);
    }
}
