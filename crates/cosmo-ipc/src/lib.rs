//! Control-socket protocol types shared by the cosmo daemon, CLI, applet, and
//! overlay.
//!
//! The socket lives at `$XDG_RUNTIME_DIR/cosmo.sock`. Carries `Command` /
//! `Response` request types and a broadcast `Event` stream (state changes,
//! transcript partials, tool activity) for the overlay and applet to consume.
