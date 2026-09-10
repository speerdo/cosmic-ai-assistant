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

use cosmo_config::Config;
use cosmo_gate::{ConfirmResult, Gate};
use cosmo_ipc::{
    Command, ConfirmOutcome as IpcConfirmOutcome, DoctorCheck, DoctorReport, Event, PendingHold,
    Response, State, StatusInfo,
};

pub struct Engine {
    /// Config for the phase-1.4 reasoning client (read once `cosmo-reason`
    /// is wired; kept here so the engine owns exactly one copy).
    #[allow(dead_code)]
    cfg: Config,
    gate: Arc<Gate>,
    events: broadcast::Sender<Event>,
    state: Mutex<State>,
    paused: AtomicBool,
    version: &'static str,
}

impl Engine {
    pub fn new(cfg: Config, events: broadcast::Sender<Event>) -> Self {
        Self {
            cfg,
            gate: Arc::new(Gate::new()),
            events,
            state: Mutex::new(State::Idle),
            paused: AtomicBool::new(false),
            version: env!("CARGO_PKG_VERSION"),
        }
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
                    name: "reasoning".into(),
                    ok: false,
                    detail: "cosmo-reason not wired yet (phase 1 in progress)".into(),
                },
                DoctorCheck {
                    name: "agent (MCP)".into(),
                    ok: false,
                    detail: "cosmo-mcp not wired yet (phase 1 in progress)".into(),
                },
            ],
        }
    }

    /// A user turn. Phase 1: the model round trip is a stub; gate checks on
    /// locally-recognised tool verbs still run so the hold path is live.
    async fn say(&self, text: String) -> Response {
        if self.paused.load(Ordering::SeqCst) {
            return Response::Said {
                result: cosmo_ipc::TurnResult::Failed {
                    reason: "daemon is paused (cosmo toggle to resume)".into(),
                },
            };
        }

        let _turn = tracing::info_span!("turn").entered();
        self.gate.begin_turn();

        // Whole-utterance confirm: resolve locally, no model round trip.
        if let ConfirmResult::Executed(parked) = self.gate.confirm_utterance(&text) {
            self.set_state(State::Acting);
            let summary = execute_stub(&parked.tool, &parked.args);
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

        self.set_state(State::Thinking);
        // TODO(phase 1.4): cosmo-reason round trip replaces this stub.
        self.set_state(State::Idle);
        Response::Said {
            result: cosmo_ipc::TurnResult::Completed {
                reply: format!("(stub) received: {text}"),
                held: self
                    .gate
                    .pending()
                    .into_iter()
                    .map(|p| PendingHold {
                        token: p.token,
                        action: p.description,
                        parked_at_ms: p.parked_at_ms,
                    })
                    .collect(),
            },
        }
    }

    /// `cosmo confirm <token>` — executes the parked call locally (invariant
    /// #4: never a model round trip).
    async fn confirm(&self, token: String) -> Response {
        let result = self.gate.confirm_token(&token);
        match result {
            ConfirmResult::Executed(parked) => {
                self.set_state(State::Acting);
                let summary = execute_stub(&parked.tool, &parked.args);
                let _ = self.events.send(Event::HoldResolved {
                    token: parked.token.clone(),
                    executed: true,
                    summary: summary.clone(),
                });
                self.set_state(State::Idle);
                Response::Confirm(IpcConfirmOutcome::Executed {
                    token: parked.token,
                    summary,
                })
            }
            ConfirmResult::Unknown | ConfirmResult::NonePending => {
                Response::Confirm(IpcConfirmOutcome::Unknown)
            }
            ConfirmResult::NotAConfirm => Response::Confirm(IpcConfirmOutcome::Unknown),
        }
    }
}

/// Phase 1 executor placeholder: nothing dangerous runs until cosmo-mcp and
/// cosmo-tools land. Every parked call reports what *would* run.
fn execute_stub(tool: &str, args: &serde_json::Value) -> String {
    format!("would execute {tool} with {args}")
}
