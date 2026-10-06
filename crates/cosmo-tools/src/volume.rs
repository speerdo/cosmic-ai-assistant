//! `volume`: the default output's level and mute, through PipeWire's
//! `wpctl`. A reflex verb ("turn it up"), and a tool the reasoning path can
//! call, so a volume change doesn't take a model working out which keys to
//! press.
//!
//! No shell: `wpctl` is started directly with fixed arguments, and the only
//! thing taken from the caller is a whole number, clamped to 0 to 100. The
//! level never goes past 100% (`-l 1.0`), so "turn it up" can't blast.

use tokio::process::Command;

use crate::{ToolError, ToolOutput};

/// The default output.
const SINK: &str = "@DEFAULT_AUDIO_SINK@";

/// How far "up" and "down" move the level when no amount was said.
pub const STEP: u8 = 10;

/// What to do to the level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    Up(u8),
    Down(u8),
    Set(u8),
    Mute,
    Unmute,
}

impl Change {
    /// The tool's `action` word and its amount, if it takes one.
    pub fn parts(self) -> (&'static str, Option<u8>) {
        match self {
            Self::Up(n) => ("up", Some(n)),
            Self::Down(n) => ("down", Some(n)),
            Self::Set(n) => ("set", Some(n)),
            Self::Mute => ("mute", None),
            Self::Unmute => ("unmute", None),
        }
    }

    /// From the tool's arguments. `None` for `status` and anything unknown.
    pub fn parse(action: &str, amount: Option<u64>) -> Option<Self> {
        let n = amount.map(|n| n.min(100) as u8);
        Some(match action {
            "up" => Self::Up(n.unwrap_or(STEP)),
            "down" => Self::Down(n.unwrap_or(STEP)),
            "set" => Self::Set(n?),
            "mute" => Self::Mute,
            "unmute" => Self::Unmute,
            _ => return None,
        })
    }

    /// The `wpctl` argument vectors that make the change.
    fn commands(self) -> Vec<Vec<String>> {
        let volume = |level: String| {
            vec![
                "set-volume".into(),
                "-l".into(),
                "1.0".into(),
                SINK.into(),
                level,
            ]
        };
        let mute = |on: &str| vec!["set-mute".into(), SINK.into(), on.into()];
        match self {
            Self::Up(n) => vec![mute("0"), volume(format!("{n}%+"))],
            Self::Down(n) => vec![volume(format!("{n}%-"))],
            Self::Set(n) => vec![mute("0"), volume(format!("{}%", n.min(100)))],
            Self::Mute => vec![mute("1")],
            Self::Unmute => vec![mute("0")],
        }
    }
}

/// The level as `wpctl get-volume` prints it: `Volume: 0.50` or
/// `Volume: 0.50 [MUTED]`. A percentage, and whether it's muted.
pub fn parse_level(output: &str) -> Option<(u8, bool)> {
    let rest = output.trim().strip_prefix("Volume:")?;
    let level: f32 = rest.split_whitespace().next()?.parse().ok()?;
    Some((
        (level * 100.0).round().clamp(0.0, 999.0) as u8,
        rest.contains("[MUTED]"),
    ))
}

fn failed(msg: impl Into<String>) -> ToolError {
    ToolError::Failed("volume".into(), msg.into())
}

async fn wpctl(args: &[String]) -> Result<String, ToolError> {
    let out = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        Command::new("wpctl").args(args).output(),
    )
    .await
    .map_err(|_| ToolError::Timeout("volume".into(), 3))?
    .map_err(|e| {
        failed(format!(
            "couldn't run wpctl (is PipeWire's wpctl installed?): {e}"
        ))
    })?;
    if !out.status.success() {
        return Err(failed(
            String::from_utf8_lossy(&out.stderr).trim().to_owned(),
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The current level, in words.
pub async fn status() -> ToolOutput {
    let out = wpctl(&["get-volume".into(), SINK.into()]).await?;
    let (level, muted) = parse_level(&out).ok_or_else(|| failed(format!("unexpected: {out}")))?;
    Ok(describe(level, muted))
}

/// Make `change`, and say where the level ended up.
pub async fn apply(change: Change) -> ToolOutput {
    for args in change.commands() {
        wpctl(&args).await?;
    }
    status().await
}

fn describe(level: u8, muted: bool) -> String {
    if muted {
        format!("muted (the level is {level}%)")
    } else {
        format!("volume {level}%")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wpctl_levels_parse() {
        assert_eq!(parse_level("Volume: 0.92\n"), Some((92, false)));
        assert_eq!(parse_level("Volume: 0.50 [MUTED]\n"), Some((50, true)));
        assert_eq!(parse_level("nonsense"), None);
    }

    #[test]
    fn up_unmutes_and_never_passes_a_full_level() {
        let cmds = Change::Up(5).commands();
        assert_eq!(cmds[0], ["set-mute", SINK, "0"]);
        assert_eq!(cmds[1], ["set-volume", "-l", "1.0", SINK, "5%+"]);
        assert_eq!(Change::Set(250).commands().last().unwrap()[4], "100%");
    }

    #[test]
    fn the_tools_words_round_trip() {
        for c in [
            Change::Up(10),
            Change::Down(5),
            Change::Set(40),
            Change::Mute,
            Change::Unmute,
        ] {
            let (action, n) = c.parts();
            assert_eq!(Change::parse(action, n.map(u64::from)), Some(c));
        }
        assert_eq!(Change::parse("up", None), Some(Change::Up(STEP)));
        assert_eq!(Change::parse("set", None), None, "a level needs a number");
        assert_eq!(Change::parse("status", None), None);
    }
}
