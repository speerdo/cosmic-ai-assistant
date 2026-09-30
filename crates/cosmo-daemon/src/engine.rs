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
use cosmo_reason::secret::DefaultKeySource;
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
#[derive(Debug, Clone, Copy)]
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
        // The session of this uid: ask logind for sessions and pick ours by
        // leader being in our session — simpler: use $XDG_SESSION_ID when
        // present, else the first active graphical session of our uid.
        let session_path = match std::env::var("XDG_SESSION_ID") {
            Ok(id) if !id.is_empty() => format!("/org/freedesktop/login1/session/{id}"),
            _ => anyhow::bail!("XDG_SESSION_ID unset; cannot locate session for LockedHint"),
        };
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
}

impl Engine {
    pub async fn new(cfg: Config, events: broadcast::Sender<Event>) -> Self {
        let logind = LogindLock::connect().await.ok();
        let lock_mode = match std::env::var("XDG_CURRENT_DESKTOP").as_deref() {
            Ok(d) if d.eq_ignore_ascii_case("COSMIC") => LockMode::CosmicDenyAll,
            _ => LockMode::LogindHint,
        };
        Self {
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
            let barge = if self.cfg.barge_in {
                "; ⚠ barge_in is on: the mic stays open while cosmo speaks, so on \
                 speakers it will hear itself (use a headset, or set barge_in: false)"
            } else {
                ""
            };
            return DoctorCheck {
                name: "ears".into(),
                ok: models_ok && capture_ok && keyboards > 0 && !self.cfg.barge_in,
                detail: format!("{capture}; {key}; {models}{barge}"),
            };
        }
        DoctorCheck {
            name: "ears".into(),
            ok: false,
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
        let host = DaemonToolHost::new(
            Arc::new(host),
            &tmux_session,
            cosmo_tools::announce::Announcer::with_delivery(Arc::new(AnnounceDelivery {
                speech: self.speech.clone(),
            })),
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

    /// Refresh the gate's lock state according to the active policy and
    /// log (once per call) the decision. Called at startup and before each
    /// turn.
    pub fn refresh_lock(&self) {
        let state = match self.lock_mode {
            // Findings §L: no unprivileged lock source on COSMIC today —
            // sensitive tools are denied outright (gate denies `Unknown`).
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

    /// Handle one command. This is the single entry point from the socket.
    pub async fn handle(&self, cmd: Command) -> Response {
        tracing::debug!(?cmd, "command");
        match cmd {
            Command::Status => Response::Status(self.status()),
            Command::Doctor => Response::Doctor(self.doctor()),
            Command::Say { text } => self.say(text).await,
            Command::Confirm { token } => self.confirm(token).await,
            Command::Cancel { token } => {
                let ok = self.gate.reject(&token);
                if ok {
                    let _ = self.events.send(Event::HoldResolved {
                        token: token.clone(),
                        executed: false,
                        summary: "rejected".into(),
                    });
                    // Nothing left to wait on: leave Waiting, or the next
                    // turn starts from a state that is no longer true.
                    if self.gate.pending().is_empty() && self.state() == State::Waiting {
                        self.set_state(State::Idle);
                    }
                }
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
            Command::Listen => self.listen(),
            Command::Toggle => {
                let paused = !self.paused.load(Ordering::SeqCst);
                self.paused.store(paused, Ordering::SeqCst);
                Response::Toggled { paused }
            }
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

    fn doctor(&self) -> DoctorReport {
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
                "agent not connected — is computer-use-linux installed? (plan §1.3)".to_owned(),
            ),
        };
        let (lock_ok, lock_detail) = match (self.lock_mode, self.gate.lock_state()) {
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
            (LockMode::CosmicDenyAll, _) => (
                false,
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
        let key_detail = if std::env::var("OPENAI_API_KEY")
            .map(|k| !k.trim().is_empty())
            .unwrap_or(false)
        {
            (true, "key present (source: env — dev/CI only)".to_owned())
        } else {
            (
                false,
                "no key in env; keyring checked at first turn".to_owned(),
            )
        };
        DoctorReport {
            checks: vec![
                DoctorCheck {
                    name: "config".into(),
                    ok: cfg_ok,
                    detail: if cfg_ok {
                        "config.ron loads".into()
                    } else {
                        "config.ron unreadable — delete it to regenerate defaults".into()
                    },
                },
                DoctorCheck {
                    name: "control socket".into(),
                    ok: socket_ok,
                    detail: "the daemon is listening (you are talking to it)".into(),
                },
                DoctorCheck {
                    name: "lock policy".into(),
                    ok: lock_ok,
                    detail: lock_detail,
                },
                DoctorCheck {
                    name: "api key".into(),
                    ok: key_detail.0,
                    detail: key_detail.1,
                },
                DoctorCheck {
                    name: "reasoning".into(),
                    ok: true,
                    detail: "cosmo-reason wired (chat-completions v1)".into(),
                },
                DoctorCheck {
                    name: "agent (MCP)".into(),
                    ok: agent_ok,
                    detail: agent_detail,
                },
                DoctorCheck {
                    name: "speech".into(),
                    ok: speech_ok,
                    detail: speech_detail,
                },
                self.ears_check(),
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
            match cosmo_reason::Reasoner::new(Arc::new(self.cfg.clone()), &DefaultKeySource) {
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
        reasoner.set_memory(&memory);

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
