//! What reflex can do: the **only** verbs, each a safe one (blueprint §7's
//! reflex exception). Anything else escalates to reasoning, where the gate
//! applies.

use crate::apps::AppRef;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaCommand {
    Play,
    Pause,
    Stop,
    Next,
    Previous,
}

impl MediaCommand {
    /// The `media_control` tool's command word.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Play => "play",
            Self::Pause => "pause",
            Self::Stop => "stop",
            Self::Next => "next",
            Self::Previous => "previous",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Intent {
    Media(MediaCommand),
    /// Start an installed application.
    Launch(AppRef),
    /// Bring a running application's window forward.
    Focus(AppRef),
    SwitchWorkspace(u32),
    /// Move the focused window to a workspace.
    MoveToWorkspace(u32),
    Maximize,
    Minimize,
}

impl Intent {
    /// The tool call this intent is, as the gate sees it. The gate
    /// integration test holds every one of these to `Allow`.
    pub fn tool_call(&self) -> (&'static str, serde_json::Value) {
        use serde_json::json;
        match self {
            Self::Media(c) => ("media_control", json!({ "command": c.as_str() })),
            Self::Launch(app) => ("launch_app", json!({ "app": app.id })),
            Self::Focus(app) => ("focus_app", json!({ "app": app.id })),
            Self::SwitchWorkspace(n) => ("switch_workspace", json!({ "workspace": n })),
            Self::MoveToWorkspace(n) => ("move_window_to_workspace", json!({ "workspace": n })),
            Self::Maximize => ("maximize_window", json!({})),
            Self::Minimize => ("minimize_window", json!({})),
        }
    }

    /// In plain words, for results and logs ("launch Firefox").
    pub fn describe(&self) -> String {
        match self {
            Self::Media(c) => format!("{} media", c.as_str()),
            Self::Launch(app) => format!("launch {}", app.name),
            Self::Focus(app) => format!("focus {}", app.name),
            Self::SwitchWorkspace(n) => format!("switch to workspace {n}"),
            Self::MoveToWorkspace(n) => format!("move this window to workspace {n}"),
            Self::Maximize => "maximize this window".into(),
            Self::Minimize => "minimize this window".into(),
        }
    }

    /// How the gate should see every reflex tool: a UI-state mutator that
    /// destroys nothing (MCP `destructiveHint: false`). Passed explicitly,
    /// because the gate treats an unannotated tool as destructive.
    pub const READ_ONLY: bool = false;
    pub const DESTRUCTIVE: bool = false;

    /// The cached phrase acknowledging it, if any. Media commands answer for
    /// themselves: the music stopping is the ack.
    pub fn ack(&self) -> Option<&'static str> {
        match self {
            Self::Media(_) => None,
            Self::Launch(_) => Some("ack-launching"),
            Self::Focus(_) | Self::SwitchWorkspace(_) | Self::Maximize | Self::Minimize => {
                Some("ack-focused")
            }
            Self::MoveToWorkspace(_) => Some("ack-moving"),
        }
    }
}
