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

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::broadcast;
use tracing::Instrument;

use cosmo_config::Config;
use cosmo_gate::{ConfirmResult, Gate, LockSource, LockState};
use cosmo_mcp::McpHost;
use cosmo_reason::secret::DefaultKeySource;
use cosmo_reason::tools::ToolHost;

use crate::toolhost::DaemonToolHost;

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
    Response, State, StatusInfo,
};

pub struct Engine {
    cfg: Config,
    gate: Arc<Gate>,
    events: broadcast::Sender<Event>,
    state: Mutex<State>,
    paused: AtomicBool,
    version: &'static str,
    /// Lock policy mode from findings §L: `Logind` on GNOME (hint flips),
    /// `DenyAll` on COSMIC (logind stays `no` even when locked — upstream
    /// greeter never sets it — so sensitive tools are denied outright and
    /// the logind probe is not trusted).
    lock_mode: LockMode,
    logind: Option<LogindLock>,
    /// Combined agent + native tool host (None until the agent connects).
    tools: Mutex<Option<Arc<DaemonToolHost>>>,
    /// Conversation history for the reasoning loop (compacted per turn by
    /// keeping only the tail; phase 5 replaces with the Realtime session).
    history: Mutex<Vec<serde_json::Value>>,
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
            events,
            state: Mutex::new(State::Idle),
            paused: AtomicBool::new(false),
            version: env!("CARGO_PKG_VERSION"),
            lock_mode,
            logind,
            tools: Mutex::new(None),
            history: Mutex::new(Vec::new()),
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
        *self.tools.lock().unwrap() =
            Some(Arc::new(DaemonToolHost::new(Arc::new(host), &tmux_session)));
        Ok(())
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
        *self.state.lock().unwrap() = state;
        let _ = self.events.send(Event::State { state });
    }

    fn state(&self) -> State {
        *self.state.lock().unwrap()
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
                }
                Response::Cancelled {
                    ok,
                    reason: (!ok).then(|| "no pending hold with that token".to_string()),
                }
            }
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
            Some(host) => {
                let n = host.agent_tool_count();
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
            ],
        }
    }

    /// A user turn: whole-utterance confirms resolve locally; everything
    /// else runs the reasoning tool loop.
    async fn say(&self, text: String) -> Response {
        if self.paused.load(Ordering::SeqCst) {
            return Response::Said {
                result: cosmo_ipc::TurnResult::Failed {
                    reason: "daemon is paused (cosmo toggle to resume)".into(),
                },
            };
        }

        let turn_span = tracing::info_span!("turn");
        self.gate.begin_turn();
        // Refresh lock state before every turn (plan §1.2: between turns
        // and on lock-source signals).
        self.refresh_lock();

        // Whole-utterance confirm: resolve locally, no model round trip.
        if let ConfirmResult::Executed(parked) = self.gate.confirm_utterance(&text) {
            self.set_state(State::Acting);
            let summary = self.execute_parked(&parked.tool, &parked.args).await;
            let _ = self.events.send(Event::HoldResolved {
                token: parked.token.clone(),
                executed: true,
                summary: summary.clone(),
            });
            self.set_state(State::Idle);
            return Response::Said {
                result: cosmo_ipc::TurnResult::ConfirmedLocally {
                    token: parked.token,
                    summary,
                },
            };
        }

        // Reasoning path.
        let Some(tools) = self.tools.lock().unwrap().clone() else {
            return Response::Said {
                result: cosmo_ipc::TurnResult::Failed {
                    reason: "agent not connected — check `cosmo doctor`".into(),
                },
            };
        };

        // Key resolution is lazy (first reasoning turn) — a locked keyring
        // at boot must not have killed the daemon (plan §1.4).
        let key_source = DefaultKeySource;
        let mut reasoner =
            match cosmo_reason::Reasoner::new(Arc::new(self.cfg.clone()), &key_source) {
                Ok(r) => r,
                Err(cosmo_reason::ReasonError::NoKey(msg)) => {
                    return Response::Said {
                        result: cosmo_ipc::TurnResult::Failed { reason: msg },
                    };
                }
                Err(e) => {
                    return Response::Said {
                        result: cosmo_ipc::TurnResult::Failed {
                            reason: e.to_string(),
                        },
                    };
                }
            };

        self.set_state(State::Thinking);
        let mut history = self.history.lock().unwrap().clone();
        let outcome = reasoner
            .turn(&text, &self.gate, tools.as_ref(), &mut history)
            .instrument(turn_span)
            .await;
        // Persist a bounded tail of the history (phase 5 replaces this).
        {
            let mut hist = self.history.lock().unwrap();
            *hist = history;
            let tail_from = tail_start(&hist, 20);
            hist.drain(0..tail_from);
        }

        self.set_state(State::Idle);
        match outcome {
            Ok(cosmo_reason::ToolOutcome::Reply(reply)) => {
                let _ = self.events.send(Event::Reply {
                    text: reply.clone(),
                });
                Response::Said {
                    result: cosmo_ipc::TurnResult::Completed {
                        reply,
                        held: self.pending_ipc_holds(),
                    },
                }
            }
            Ok(cosmo_reason::ToolOutcome::Held { token, tool }) => {
                self.set_state(State::Waiting);
                let action = format!("{tool} — confirm with: cosmo confirm {token}");
                let _ = self.events.send(Event::Held {
                    token: token.clone(),
                    action: action.clone(),
                });
                Response::Said {
                    result: cosmo_ipc::TurnResult::Completed {
                        reply: action,
                        held: self.pending_ipc_holds(),
                    },
                }
            }
            Ok(cosmo_reason::ToolOutcome::ToolRan { .. }) => Response::Said {
                result: cosmo_ipc::TurnResult::Completed {
                    reply: "tool ran".into(),
                    held: self.pending_ipc_holds(),
                },
            },
            Err(cosmo_reason::ReasonError::NoKey(msg)) => Response::Said {
                result: cosmo_ipc::TurnResult::Failed { reason: msg },
            },
            Err(e) => Response::Said {
                result: cosmo_ipc::TurnResult::Failed {
                    reason: e.to_string(),
                },
            },
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
            ConfirmResult::NotAConfirm => Response::Confirm {
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
