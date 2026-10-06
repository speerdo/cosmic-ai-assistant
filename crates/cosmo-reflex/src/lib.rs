//! The reflex path: local intent matching over the ~30-phrase command
//! vocabulary plus app names, confidence scoring, normalization, and
//! escalation.
//!
//! **Escalation rule:** reflex first, always. Below confidence threshold,
//! hand the transcript to reasoning. If reflex matched but the action failed,
//! escalate rather than report failure.
//!
//! ## Invariant
//!
//! Reflex executes only allowlisted safe verbs (media and volume control,
//! focus/launch, workspace moves). Nothing on the gate's deny or hold lists is reachable
//! without the model — the fast path can never become the unsafe path.
//! [`Intent`] *is* that allowlist: there is no variant for anything else,
//! and `tests/gate.rs` holds every variant's tool call to the gate's
//! `Allow`.

mod apps;
mod intent;
mod matcher;
mod normalize;

pub use apps::{AppIndex, AppMatch, AppRef};
pub use intent::{Ask, Intent, MediaCommand, VolumeCommand};
pub use matcher::{Match, Matcher, THRESHOLD};
pub use normalize::words;

#[cfg(test)]
mod tests;
