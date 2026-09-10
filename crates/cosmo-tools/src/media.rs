//! `media_control`: MPRIS over zbus (blueprint §6). Reflex-path candidate.

use crate::{ToolError, ToolOutput};

/// Commands accepted. Anything else is refused before any bus call.
pub const COMMANDS: &[&str] = &["play", "pause", "play_pause", "next", "previous", "stop"];

/// Wire the MPRIS calls lazily; phase 1 ships the command vocabulary and
/// errors clearly when no player is reachable. The zbus integration lands
/// with the reflex phase (§4) where this is first needed — the command
/// check is here from day one so the gate mapping is stable.
pub async fn control(cmd: &str) -> ToolOutput {
    if !COMMANDS.contains(&cmd) {
        return Err(ToolError::Failed(
            "media_control".into(),
            format!(
                "unknown command `{cmd}`; expected one of {}",
                COMMANDS.join(", ")
            ),
        ));
    }
    Err(ToolError::Failed(
        "media_control".into(),
        "no MPRIS player reachable (zbus wiring lands with the reflex phase)".into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn unknown_commands_refused() {
        let err = control("rm -rf /").await.expect_err("must refuse");
        assert!(err.to_string().contains("unknown command"));
        let err = control("volume 100").await.expect_err("must refuse");
        assert!(err.to_string().contains("unknown command"));
    }
}
