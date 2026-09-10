//! The COSMIC panel applet: a thin libcosmic client over the control socket.
//!
//! ## Invariant
//!
//! The panel spawns one applet process per output. The applet owns nothing —
//! the daemon owns the mic, hotkey, and resident models. See the
//! `cosmo-daemon` crate docs before being tempted to move state here.
