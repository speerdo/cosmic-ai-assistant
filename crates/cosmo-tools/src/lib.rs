//! Native tools exposed to the model alongside MCP tools.
//!
//! - `run_in_terminal` / `read_terminal` / `watch_terminal`: tmux
//!   `capture-pane` and `pane_current_command`. The shell reappearing is an
//!   unambiguous done signal. The single most useful behavior in the project.
//! - `announce`: the only unprompted speech. Waits for any in-flight reply,
//!   never lands closer than 8s apart, degrades to a notification when
//!   listening is off.
//! - `remember`: flat file, the only memory spanning two sittings.
//! - `system_query`: `df`, `ip`, `free`, `systemctl`, sensors. Read-only
//!   allowlist, no shell.
//! - `clipboard`: `wl-clipboard-rs`. Reading leaves the machine, so gate it.
//! - `media_control`: MPRIS over `zbus`. Reflex-path candidate.
