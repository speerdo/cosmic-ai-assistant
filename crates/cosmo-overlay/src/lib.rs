//! The overlay: a wlr-layer-shell surface anchored bottom-center — no
//! decorations, no focus steal. The face; a panel dot is not a UI.
//!
//! States, in build order: listening (waveform + live partial), thinking,
//! acting (tool name in plain words), waiting (pending action plus confirm
//! affordance), speaking, idle. All driven by IPC events from the daemon.
//!
//! Redraw discipline: coalesce twice — once across the burst of events a
//! single change produces, and again on `wl_surface.frame` — so one input is
//! at most one buffer. Degrades to notifications where layer shell is absent
//! (GNOME).
