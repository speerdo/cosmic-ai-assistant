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

/// A change to the output volume. Amounts are percentage points (or, for
/// `Set`, the level), 0 to 100.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VolumeCommand {
    Up(u8),
    Down(u8),
    Set(u8),
    Mute,
    Unmute,
}

impl VolumeCommand {
    /// The `volume` tool's `action` word and its `amount`, if it has one.
    pub fn parts(self) -> (&'static str, Option<u8>) {
        match self {
            Self::Up(n) => ("up", Some(n)),
            Self::Down(n) => ("down", Some(n)),
            Self::Set(n) => ("set", Some(n)),
            Self::Mute => ("mute", None),
            Self::Unmute => ("unmute", None),
        }
    }

    /// The inverse of [`parts`](Self::parts), for the tool's arguments.
    /// Up and down default to a step of 10; a level needs a number.
    pub fn parse(action: &str, amount: Option<u64>) -> Option<Self> {
        let n = amount.map(|n| n.min(100) as u8);
        Some(match action {
            "up" => Self::Up(n.unwrap_or(10)),
            "down" => Self::Down(n.unwrap_or(10)),
            "set" => Self::Set(n?),
            "mute" => Self::Mute,
            "unmute" => Self::Unmute,
            _ => return None,
        })
    }
}

/// What a spoken question asks for, answered from this machine's clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ask {
    Time,
    Date,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Intent {
    Media(MediaCommand),
    Volume(VolumeCommand),
    /// "What time is it?": the answer is spoken (`speaks`).
    Ask(Ask),
    /// Start an installed application.
    Launch(AppRef),
    /// Start an application on an empty workspace.
    LaunchOnNewWorkspace(AppRef),
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
            Self::Volume(c) => {
                let (action, amount) = c.parts();
                ("volume", json!({ "action": action, "amount": amount }))
            }
            Self::Ask(Ask::Time) => ("tell_time", json!({})),
            Self::Ask(Ask::Date) => ("tell_date", json!({})),
            Self::Launch(app) => ("launch_app", json!({ "app": app.id })),
            Self::LaunchOnNewWorkspace(app) => {
                ("launch_app", json!({ "app": app.id, "new_workspace": true }))
            }
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
            Self::Ask(Ask::Time) => "tell the time".into(),
            Self::Ask(Ask::Date) => "tell the date".into(),
            Self::Volume(c) => match c.parts() {
                (action, Some(n)) => format!("volume {action} {n}"),
                (action, None) => format!("volume {action}"),
            },
            Self::Launch(app) => format!("launch {}", app.name),
            Self::LaunchOnNewWorkspace(app) => format!("launch {} on a new workspace", app.name),
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

    /// Whether what the actuator returns is the answer, to be spoken.
    pub fn speaks(&self) -> bool {
        matches!(self, Self::Ask(_))
    }

    /// The cached phrase acknowledging it, if any. Media commands answer for
    /// themselves: the music stopping is the ack.
    pub fn ack(&self) -> Option<&'static str> {
        match self {
            // Media and volume answer for themselves: you hear the change.
            Self::Media(_) | Self::Volume(_) | Self::Ask(_) => None,
            Self::Launch(_) | Self::LaunchOnNewWorkspace(_) => Some("ack-launching"),
            Self::Focus(_) | Self::SwitchWorkspace(_) | Self::Maximize | Self::Minimize => {
                Some("ack-focused")
            }
            Self::MoveToWorkspace(_) => Some("ack-moving"),
        }
    }
}
