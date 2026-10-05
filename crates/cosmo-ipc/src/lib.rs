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

#[cfg(feature = "client")]
pub mod client;

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
    /// Start listening, or stop and commit if already listening: the
    /// press-only fallback to holding the trigger key (phase 3), bindable
    /// as a COSMIC `Spawn` shortcut (`cosmo listen`).
    Listen,
    /// Voices a provider can speak (`None` = the active provider).
    VoiceList { provider: Option<String> },
    /// Speak a fixed sample line in `voice` (synthesized once, cached).
    /// Answered when the audio has finished.
    VoicePreview {
        provider: Option<String>,
        voice: String,
    },
    /// Make `voice` the active voice: persist it to `config.ron`, render
    /// its phrase cache (progress arrives as events), then switch.
    /// Answered when the switch is done or has failed.
    VoiceSet {
        provider: Option<String>,
        voice: String,
    },
    /// The reasoning providers, which one is in use, and whether each is
    /// connected (a stored key, or a local server answering).
    Reasoning,
    /// Reason with `provider` from the next turn: persisted to
    /// `config.ron`. An empty `model` is the provider's default.
    ReasoningSet { provider: String, model: String },
    /// Begin a browser sign-in. Answered at once with the URL for the
    /// asking client to open; the outcome arrives as [`Event::SignIn`].
    SignInStart { provider: String },
    /// Store a pasted API key for `provider` in the Secret Service.
    StoreKey { provider: String, key: Redacted },
}

/// A secret on the wire (the socket is the user's own, mode 0600): it
/// serializes as itself, and never prints, so logging a command can't
/// leak it.
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Redacted(pub String);

impl std::fmt::Debug for Redacted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("[redacted]")
    }
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
    Said {
        result: TurnResult,
    },
    /// Named field, not a newtype: `Response` and [`ConfirmOutcome`] are both
    /// internally tagged on `type`, so a newtype variant would serialise both
    /// tags into the *same* map — `{"type":"confirm","type":"executed",…}` —
    /// which round-trips as `duplicate field \`type\``. The action still
    /// executes; only the reply is unreadable, so the failure shows up as a
    /// client-side protocol error after the side effect has happened.
    ///
    /// Every enum-valued payload in this protocol sits under a named field
    /// for exactly this reason (cf. `Said { result }`). That is structural,
    /// where "remember to pick distinct tag names" is not.
    Confirm {
        outcome: ConfirmOutcome,
    },
    Cancelled {
        ok: bool,
        reason: Option<String>,
    },
    Toggled {
        paused: bool,
    },
    /// Whether a recording is now running after `Listen`.
    Listening {
        active: bool,
    },
    Voices {
        provider: String,
        /// The active voice id when `provider` is the active provider,
        /// resolved (`"default"` → the concrete voice).
        active: Option<String>,
        voices: Vec<VoiceInfo>,
    },
    VoicePreviewed {
        provider: String,
        voice: String,
    },
    VoiceSet {
        provider: String,
        voice: String,
        /// The config file the choice was written to.
        persisted_to: String,
    },
    Reasoning(ReasoningInfo),
    /// Reasoning switched; the next turn uses it.
    ReasoningSet {
        provider: String,
        model: String,
    },
    /// Open this in a browser to sign in to `provider`.
    SignInUrl {
        provider: String,
        url: String,
    },
    /// The key is stored.
    KeyStored {
        provider: String,
    },
    Error {
        message: String,
    },
}

/// [`Response::Reasoning`]: what's in use and what could be.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReasoningInfo {
    pub active: String,
    /// The model in use (the provider's default when none is set).
    pub model: String,
    pub providers: Vec<ProviderInfo>,
}

/// One reasoning provider, as the applet shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderInfo {
    pub name: String,
    pub label: String,
    pub connect: Connect,
    /// A key is stored, or (local) the server answers.
    pub connected: bool,
    /// Where keys are made (or, for local, where to get a server).
    pub key_page: String,
    pub default_model: String,
    /// The models offered: those a local server runs on this computer, or a
    /// cloud provider's own list (empty where it has none worth choosing
    /// from, and only its default is offered).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<String>,
    /// Models the local server lists but forwards to a cloud (Ollama's
    /// `:cloud` models): usable, but not private.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub remote_models: Vec<String>,
    /// Why it isn't connected, or a caution about its terms.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// How a provider is connected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Connect {
    /// Sign in with the browser ([`Command::SignInStart`]).
    Browser,
    /// Paste an API key ([`Command::StoreKey`]).
    Key,
    /// Nothing: a server on this machine.
    Local,
}

/// One voice in a [`Response::Voices`] listing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoiceInfo {
    pub id: String,
    pub label: String,
    /// BCP-47-ish accent code (`en-US`, `en-GB`); the listing groups by it.
    pub accent: String,
    /// `"female"` / `"male"` when the provider says.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gender: Option<String>,
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
    /// The reflex path handled it locally: a safe verb, no model.
    Reflexed {
        /// What was done, in words ("launch Firefox").
        action: String,
        /// What the action reported.
        summary: String,
    },
    /// A spoken confirm arrived with no trigger key held (open mic): it
    /// resolved nothing and went nowhere else (gate invariant #5).
    ConfirmNeedsKey,
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
    /// Works, but there's something to read: a limit by design (no lock
    /// state on COSMIC), or a setting with a cost (barge-in). Never fails
    /// `doctor`.
    #[serde(default)]
    pub warn: bool,
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
    /// Free-form progress line.
    Log { line: String },
    /// The mic's level while listening (phase 6), ~20 a second, for the
    /// overlay's waveform: RMS of the last ~50 ms, 0–1.
    Level { rms: f32 },
    /// Speech recognition (phase 3). Partials (`final: false`) are the
    /// streaming model's text so far and replace the previous partial; one
    /// final (`final: true`) ends each recording with the committed text.
    /// Transcripts are shown, not acted on, until phase 4/5.
    Transcript {
        text: String,
        r#final: bool,
        /// Release → commit, on the final only.
        #[serde(skip_serializing_if = "Option::is_none")]
        latency_ms: Option<u64>,
    },
    /// A voice's phrase cache is rendering (spec §2.6): `done` of `total`
    /// phrases are current. Fires for reused phrases too, so a consumer
    /// always sees every step.
    VoiceCacheProgress {
        provider: String,
        voice: String,
        done: u32,
        total: u32,
    },
    /// A phrase render finished. `ok = false` leaves the previous voice in
    /// place; `detail` says why.
    VoiceCacheDone {
        provider: String,
        voice: String,
        ok: bool,
        detail: String,
    },
    /// A browser sign-in finished: the key is stored (`ok`), or `detail`
    /// says why not.
    SignIn {
        provider: String,
        ok: bool,
        detail: String,
    },
}
