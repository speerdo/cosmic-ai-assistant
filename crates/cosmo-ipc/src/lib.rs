//! Control-socket protocol types shared by the cosmo daemon, CLI, applet, and
//! overlay.
//!
//! The socket lives at `$XDG_RUNTIME_DIR/cosmo.sock`. Carries `Command` /
//! `Response` request types and a broadcast `Event` stream (state changes,
//! transcript partials, tool activity) for the overlay and applet to consume.
//!
//! ## Framing
//!
//! Newline-delimited JSON (UTF-8), one message per line.
//!
//! - client → daemon: [`Request`] (a `Command` tagged with a client-chosen id)
//! - daemon → client: [`DaemonMessage`] — either the [`Response`] to a
//!   request or a broadcast [`Event`].
//!
//! Responses match request ids so a client streaming events during `say`
//! can route interleaved messages correctly.
//!
//! ## Exit codes (CLI)
//!
//! - `0` success
//! - `1` general error (the daemon reported an error, or the command failed)
//! - `2` the requested action was denied by the policy gate
//! - `3` daemon not reachable (socket missing or refused)
//! - `4` socket present but the connection misbehaved (malformed protocol,
//!   timeout waiting for a response)

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Socket file name under `$XDG_RUNTIME_DIR`.
pub const SOCKET_NAME: &str = "cosmo.sock";

/// Canonical control-socket path (`$XDG_RUNTIME_DIR/cosmo.sock`).
///
/// Falls back to `/tmp/cosmo.<uid>.sock` when `XDG_RUNTIME_DIR` is unset so
/// tests and odd environments still work; the daemon and CLI agree either way.
pub fn socket_path() -> PathBuf {
    match std::env::var("XDG_RUNTIME_DIR") {
        Ok(dir) if !dir.is_empty() => PathBuf::from(dir).join(SOCKET_NAME),
        _ => {
            let uid = uid();
            PathBuf::from(format!("/tmp/cosmo.{uid}.sock"))
        }
    }
}

/// Current uid without an `unsafe` block (`libc::getuid` would trip the
/// workspace-wide `unsafe_code` lint).
fn uid() -> u32 {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata("/proc/self")
        .map(|m| m.uid())
        .unwrap_or(1000)
}

/// Daemon-side state machine, exported over [`Event::State`].
///
/// Phase 1 fills Thinking/Acting/Waiting/Speaking from text turns; Listening
/// arrives with the audio layer (phase 3), and reflex acks (phase 4) reuse the
/// same states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum State {
    /// Doing nothing, no turn in flight.
    #[default]
    Idle,
    /// Microphone open (phase 3+). Kept now so consumers can render it.
    Listening,
    /// Model round trip in progress.
    Thinking,
    /// A tool call is executing.
    Acting,
    /// A held action awaits local confirmation.
    Waiting,
    /// Delivering the reply (spoken later; printed as text in phase 1).
    Speaking,
}

/// Client → daemon message: a command with a client-chosen correlation id.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub id: u64,
    pub cmd: Command,
}

/// Commands accepted on the control socket.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Command {
    /// Daemon status: state, pending holds, version.
    Status,
    /// Full readiness report (daemon-side view).
    Doctor,
    /// Submit a user turn.
    Say { text: String },
    /// Resolve a pending held action **locally** (invariant #4: never a model
    /// round trip).
    Confirm { token: String },
    /// Reject a pending held action without executing it.
    Cancel { token: String },
    /// Pause/resume accepting new turns.
    Toggle,
}

/// Daemon → client message: a response or a broadcast event, one per line.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DaemonMessage {
    /// Reply to a [`Request`]; `id` matches the request.
    Response { id: u64, response: Response },
    /// Broadcast event; sent to every connected client.
    Event { event: Event },
}

/// Daemon reply to a specific [`Command`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    Status(StatusInfo),
    Doctor(DoctorReport),
    Said { result: TurnResult },
    Confirm(ConfirmOutcome),
    Cancelled { ok: bool, reason: Option<String> },
    Toggled { paused: bool },
    Error { message: String },
}

/// One entry of the pending-hold list.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingHold {
    pub token: String,
    /// Human-readable description of the parked action.
    pub action: String,
    /// Unix millis when it was parked.
    pub parked_at_ms: u64,
}

/// Daemon snapshot for `cosmo status`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusInfo {
    pub state: State,
    pub paused: bool,
    pub version: String,
    pub pending_holds: Vec<PendingHold>,
}

/// Final outcome of a `say` turn.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TurnResult {
    /// Model finished; `reply` is the final text.
    Completed {
        reply: String,
        /// Tokens held during the turn (they still await confirmation).
        held: Vec<PendingHold>,
    },
    /// The turn was a whole-utterance confirm and resolved a pending hold
    /// locally — no model was contacted.
    ConfirmedLocally {
        token: String,
        /// What the confirmed action reported.
        summary: String,
    },
    /// The turn was a whole-utterance confirm but no hold matched.
    ConfirmIgnored,
    /// The turn could not run (paused daemon, model error, …).
    Failed { reason: String },
}

/// Outcome of `cosmo confirm <token>`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ConfirmOutcome {
    /// Held action executed locally. No model round trip occurred.
    Executed { token: String, summary: String },
    /// No pending hold with that token.
    Unknown,
    /// The hold was already resolved or rejected.
    AlreadyGone { reason: String },
}

/// One readiness row for `cosmo doctor`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoctorCheck {
    pub name: String,
    pub ok: bool,
    /// Short explanation when not ok, or extra detail when ok.
    pub detail: String,
}

/// Daemon-side readiness report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoctorReport {
    pub checks: Vec<DoctorCheck>,
}

/// Broadcast events. The overlay (phase 6) and applet render these; the CLI
/// prints them for `say` transparency.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    /// State machine transition.
    State { state: State },
    /// A tool call started.
    ToolStarted {
        call_id: String,
        tool: String,
        args: String,
    },
    /// A tool call finished; `latency_ms` is the executor-measured duration.
    ToolFinished {
        call_id: String,
        tool: String,
        ok: bool,
        latency_ms: u64,
        summary: String,
    },
    /// A gated call was parked awaiting local confirmation.
    Held { token: String, action: String },
    /// A parked hold was resolved (confirmed → executed, or rejected).
    HoldResolved {
        token: String,
        executed: bool,
        summary: String,
    },
    /// Final reply text for the turn (what the overlay will later speak).
    Reply { text: String },
    /// Per-turn model usage + server-reported rate limit (invariant #7:
    /// logged every turn).
    Usage {
        prompt_tokens: u64,
        completion_tokens: u64,
        total_tokens: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        remaining_requests: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        remaining_tokens: Option<u64>,
    },
    /// Free-form progress line (phase 1 stand-in for transcript partials).
    Log { line: String },
}
