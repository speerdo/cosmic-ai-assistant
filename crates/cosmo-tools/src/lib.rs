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

pub mod announce;
pub mod media;
pub mod memory;
pub mod system;
pub mod terminal;

/// The canonical native-tool names. `cosmo-mcp` registers these alongside
/// agent tools; the reasoning loop sees one flat namespace.
pub mod registry {
    /// Native tool names (plan §1.3).
    pub const NAMES: &[&str] = &[
        "run_in_terminal",
        "read_terminal",
        "watch_terminal",
        "announce",
        "remember",
        "system_query",
        "clipboard_get",
        "clipboard_set",
        "media_control",
    ];
}

/// Result of a native tool execution: text back to the model.
pub type ToolOutput = Result<String, ToolError>;

#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("tool `{0}` failed: {1}")]
    Failed(String, String),
    #[error("tool `{0}` timed out after {1}s")]
    Timeout(String, u64),
}

pub use terminal::Terminal;
