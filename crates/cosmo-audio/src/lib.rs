//! Native PipeWire audio: continuous capture into a pre-roll ring buffer,
//! VAD-based segment cuts, and playback of cached or freshly synthesized PCM.
//!
//! ## Invariants
//!
//! - A native PipeWire client, not `pw-record`: the callback rides the RT
//!   data-loop and keeps being serviced when the machine is saturated.
//! - Capture continuously (~750ms+ pre-roll) so the first syllable survives
//!   key-press latency.
//! - Half-duplex by default: the mic is held shut while speaking plus ~350ms
//!   for the room to settle, gated at the ring buffer. Barge-in stays behind
//!   config with a `doctor` warning.
//!
//! ## What exists today (spec §2.3)
//!
//! The speaker half only. Capture is phase 3; the half-duplex *hook* it will
//! gate on — [`SpeechGate`] — exists now so phase 3 does not have to find
//! the seam.
//!
//! - [`Clip`] — what playback accepts: mono `f32` at the clip's own rate.
//!   This crate does not depend on `cosmo-tts`; `Pcm`'s two public fields
//!   map onto a clip one to one.
//! - `Player` (feature `pipewire-backend`) — a dedicated PipeWire thread
//!   that queues clips, plays them gaplessly, and resolves each clip's
//!   [`Playback`] handle once its audio has actually drained.

mod clip;
mod gate;
// The queue's main-loop half is only called by the PipeWire player; the
// core tier still compiles and unit-tests it without the backend.
#[cfg_attr(not(feature = "pipewire-backend"), allow(dead_code))]
mod queue;

#[cfg(feature = "pipewire-backend")]
mod player;

pub use clip::Clip;
pub use gate::{SETTLE, SpeechGate};
#[cfg(feature = "pipewire-backend")]
pub use player::Player;
pub use queue::{Outcome, Playback};

/// Everything playback can report. Carries enough to render an actionable
/// `doctor` line — "PipeWire not running" is a different fix from "the
/// output device went away mid-sentence".
#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum AudioError {
    /// The session's PipeWire daemon could not be reached.
    #[error("cannot connect to PipeWire: {0}")]
    Connect(String),
    /// The playback stream could not be created or went into error.
    #[error("playback stream failed: {0}")]
    Stream(String),
    /// The clip is unplayable as given (zero rate, and so on).
    #[error("invalid clip: {0}")]
    InvalidClip(String),
    /// The player thread is gone — shut down, or it died.
    #[error("the playback thread is not running")]
    Closed,
}
