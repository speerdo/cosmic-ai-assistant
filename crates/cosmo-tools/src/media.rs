//! `media_control`: MPRIS over zbus (blueprint §6). The reflex path's media
//! verbs, and a tool the reasoning path can call.
//!
//! Which player a command goes to is decided by [`targets`], a pure
//! function of the players' playback states: "pause" and "stop" silence
//! everything playing, "play" resumes what cosmo paused (else whatever is
//! paused), and "next" / "previous" go to the player that's playing.
//! "status" only reports.
//!
//! `playerctld` is ignored: it's a proxy that mirrors another player, so
//! counting it would send each command twice (a toggle would cancel out)
//! or to the wrong player.

use std::collections::HashMap;

use crate::{ToolError, ToolOutput};

/// Commands accepted. Anything else is refused before any bus call.
pub const COMMANDS: &[&str] = &[
    "play",
    "pause",
    "play_pause",
    "next",
    "previous",
    "stop",
    "status",
];

/// The players cosmo last paused or stopped, so "play" resumes those
/// rather than whichever paused player the bus lists first.
static LAST_PAUSED: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

const PREFIX: &str = "org.mpris.MediaPlayer2.";
const PATH: &str = "/org/mpris/MediaPlayer2";
const PLAYER: &str = "org.mpris.MediaPlayer2.Player";

/// A player's `PlaybackStatus`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Playing,
    Paused,
    Stopped,
}

impl Status {
    fn parse(s: &str) -> Self {
        match s {
            "Playing" => Self::Playing,
            "Paused" => Self::Paused,
            _ => Self::Stopped,
        }
    }
}

/// The players `cmd` should go to, given every player's status. Empty when
/// there is nothing sensible to do (pausing when nothing plays).
pub fn targets<'a>(
    cmd: &str,
    players: &'a [(String, Status)],
    paused_by_us: &[String],
) -> Vec<&'a str> {
    // Resuming: what we paused, while it's still paused.
    let ours: Vec<&str> = players
        .iter()
        .filter(|p| p.1 == Status::Paused && paused_by_us.contains(&p.0))
        .map(|p| p.0.as_str())
        .collect();
    let with = |s: Status| {
        players
            .iter()
            .filter(move |p| p.1 == s)
            .map(|p| p.0.as_str())
    };
    let first = |order: &[Status]| {
        order
            .iter()
            .find_map(|s| with(*s).next())
            .into_iter()
            .collect::<Vec<_>>()
    };
    match cmd {
        "pause" | "stop" => with(Status::Playing).collect(),
        "play" if !ours.is_empty() => ours,
        "play" => first(&[Status::Paused, Status::Stopped]),
        // Toggling: whatever plays is paused; otherwise resume one.
        "play_pause" => {
            let playing: Vec<_> = with(Status::Playing).collect();
            if playing.is_empty() && !ours.is_empty() {
                ours
            } else if playing.is_empty() {
                first(&[Status::Paused, Status::Stopped])
            } else {
                playing
            }
        }
        "next" | "previous" => first(&[Status::Playing, Status::Paused, Status::Stopped]),
        _ => Vec::new(),
    }
}

fn method(cmd: &str) -> &'static str {
    match cmd {
        "play" => "Play",
        "pause" => "Pause",
        "play_pause" => "PlayPause",
        "next" => "Next",
        "previous" => "Previous",
        _ => "Stop",
    }
}

fn failed(msg: impl Into<String>) -> ToolError {
    ToolError::Failed("media_control".into(), msg.into())
}

/// Every running MPRIS player and its status. Only names currently owned
/// on the bus: an activatable-only player (`playerctld`) is not started.
pub async fn players(conn: &zbus::Connection) -> Result<Vec<(String, Status)>, ToolError> {
    let bus = zbus::fdo::DBusProxy::new(conn)
        .await
        .map_err(|e| failed(format!("session bus: {e}")))?;
    let names = bus
        .list_names()
        .await
        .map_err(|e| failed(format!("session bus: {e}")))?;
    let mut out = Vec::new();
    for name in names
        .iter()
        .map(|n| n.to_string())
        .filter(|n| n.starts_with(PREFIX) && !n.starts_with(&format!("{PREFIX}playerctld")))
    {
        // A player that won't answer is skipped, not fatal.
        if let Ok(status) = status(conn, &name).await {
            out.push((name, status));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

async fn status(conn: &zbus::Connection, name: &str) -> zbus::Result<Status> {
    let props = zbus::fdo::PropertiesProxy::builder(conn)
        .destination(name.to_owned())?
        .path(PATH)?
        .build()
        .await?;
    let iface = zbus::names::InterfaceName::from_static_str_unchecked(PLAYER);
    let value = props.get(iface, "PlaybackStatus").await?;
    let s: String = value.try_into().map_err(zbus::Error::Variant)?;
    Ok(Status::parse(&s))
}

/// Send `cmd` to one player by bus name.
pub async fn send(conn: &zbus::Connection, name: &str, cmd: &str) -> Result<(), ToolError> {
    conn.call_method(Some(name), PATH, Some(PLAYER), method(cmd), &())
        .await
        .map(|_| ())
        .map_err(|e| failed(format!("{}: {e}", short(name))))
}

fn short(name: &str) -> &str {
    name.strip_prefix(PREFIX).unwrap_or(name)
}

pub async fn control(cmd: &str) -> ToolOutput {
    if !COMMANDS.contains(&cmd) {
        return Err(failed(format!(
            "unknown command `{cmd}`; expected one of {}",
            COMMANDS.join(", ")
        )));
    }
    let conn = zbus::Connection::session()
        .await
        .map_err(|e| failed(format!("session bus: {e}")))?;
    let players = players(&conn).await?;
    if players.is_empty() {
        return Err(failed("no media player is running"));
    }
    let states: HashMap<_, _> = players.iter().map(|(n, s)| (short(n), *s)).collect();
    if cmd == "status" {
        return Ok(format!("players: {states:?}"));
    }
    let paused_by_us = LAST_PAUSED.lock().unwrap().clone();
    let targets = targets(cmd, &players, &paused_by_us);
    if targets.is_empty() {
        return Ok(format!("nothing to {cmd}: {states:?}"));
    }
    for name in &targets {
        send(&conn, name, cmd).await?;
    }
    if matches!(cmd, "pause" | "stop") {
        *LAST_PAUSED.lock().unwrap() = targets.iter().map(|n| n.to_string()).collect();
    }
    let names: Vec<_> = targets.iter().map(|n| short(n)).collect();
    Ok(format!("{cmd}: {}", names.join(", ")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn play_resumes_what_cosmo_paused_first() {
        use Status::*;
        let players = p(&[("edge", Paused), ("spotify", Paused)]);
        assert_eq!(targets("play", &players, &[]), ["edge"], "bus order alone");
        let ours = ["spotify".to_string()];
        assert_eq!(targets("play", &players, &ours), ["spotify"]);
        assert_eq!(targets("play_pause", &players, &ours), ["spotify"]);
        // Ours started playing again elsewhere: back to any paused one.
        let players = p(&[("edge", Paused), ("spotify", Playing)]);
        assert!(targets("play", &players, &ours) == ["edge"]);
    }

    #[tokio::test]
    async fn unknown_commands_refused() {
        let err = control("rm -rf /").await.expect_err("must refuse");
        assert!(err.to_string().contains("unknown command"));
        let err = control("volume 100").await.expect_err("must refuse");
        assert!(err.to_string().contains("unknown command"));
    }

    fn p(list: &[(&str, Status)]) -> Vec<(String, Status)> {
        list.iter().map(|(n, s)| (n.to_string(), *s)).collect()
    }

    #[test]
    fn pause_silences_everything_playing() {
        use Status::*;
        let players = p(&[("a", Playing), ("b", Paused), ("c", Playing)]);
        assert_eq!(targets("pause", &players, &[]), ["a", "c"]);
        assert_eq!(targets("stop", &players, &[]), ["a", "c"]);
        assert!(targets("pause", &p(&[("b", Paused)]), &[]).is_empty());
    }

    #[test]
    fn play_resumes_the_paused_one() {
        use Status::*;
        let players = p(&[("a", Stopped), ("b", Paused)]);
        assert_eq!(targets("play", &players, &[]), ["b"]);
        assert_eq!(targets("play", &p(&[("a", Stopped)]), &[]), ["a"]);
        assert!(
            targets("play", &p(&[("a", Playing)]), &[]).is_empty(),
            "already playing"
        );
    }

    #[test]
    fn next_goes_to_the_one_playing() {
        use Status::*;
        let players = p(&[("a", Paused), ("b", Playing)]);
        assert_eq!(targets("next", &players, &[]), ["b"]);
        assert_eq!(targets("previous", &p(&[("a", Paused)]), &[]), ["a"]);
    }

    #[test]
    fn toggle_pauses_what_plays_or_resumes() {
        use Status::*;
        assert_eq!(
            targets("play_pause", &p(&[("a", Playing), ("b", Paused)]), &[]),
            ["a"]
        );
        assert_eq!(targets("play_pause", &p(&[("b", Paused)]), &[]), ["b"]);
    }
}
