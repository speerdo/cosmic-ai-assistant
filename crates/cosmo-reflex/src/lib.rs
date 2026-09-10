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
//! Reflex executes only allowlisted safe verbs (media control, focus/launch,
//! workspace moves). Nothing on the gate's deny or hold lists is reachable
//! without the model — the fast path can never become the unsafe path.
