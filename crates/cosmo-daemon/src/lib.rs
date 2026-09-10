//! The cosmo daemon: a single long-lived process running the state machine
//! (`Idle / Listening / Thinking / Acting / Waiting / Speaking`) that owns
//! everything — microphone, hotkey, resident models, tools, and the control
//! socket.
//!
//! ## Invariant
//!
//! The COSMIC panel spawns **one applet process per output**. The daemon is a
//! separate process precisely so that multiplicity doesn't matter: the applet
//! and overlay are thin clients over the control socket and never own engine
//! state. Do not "simplify" by moving the engine into the applet.
