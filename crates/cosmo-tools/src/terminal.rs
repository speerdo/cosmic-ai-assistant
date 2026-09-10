//! tmux terminal tools: `run_in_terminal`, `read_terminal`,
//! `watch_terminal`.
//!
//! Done signal: `pane_current_command` returning to the shell means the
//! command finished (blueprint §6) — no exit-code scraping, no polling of
//! log files. `capture-pane` is the transcript.
//!
//! A dedicated tmux server (`-S cosmo`) keeps cosmo's panes separate from
//! the user's own sessions; every invocation passes args verbatim — there
//! is no shell between cosmo and tmux.

use std::time::Duration;

use tokio::process::Command;
use tokio::time::timeout;

use crate::{ToolError, ToolOutput};

/// One tmux session owned by cosmo (config `tmux_session`).
pub struct Terminal {
    session: String,
    call_timeout: Duration,
}

impl Terminal {
    pub fn new(session: impl Into<String>) -> Self {
        Self {
            session: session.into(),
            call_timeout: Duration::from_secs(120),
        }
    }

    pub fn with_timeout(mut self, t: Duration) -> Self {
        self.call_timeout = t;
        self
    }

    fn tmux(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new("tmux");
        // Dedicated socket: never touch the user's tmux server.
        cmd.arg("-S").arg("cosmo");
        cmd.args(args);
        cmd
    }

    /// Does the pane run a command (i.e. not back at the shell)?
    async fn command_running(&self) -> ToolOutput {
        let out = self
            .tmux(&[
                "display-message",
                "-p",
                "-t",
                &self.session,
                "#{pane_current_command}",
            ])
            .kill_on_drop(true)
            .output()
            .await
            .map_err(|e| ToolError::Failed("watch_terminal".into(), e.to_string()))?;
        if !out.status.success() {
            return Err(ToolError::Failed(
                "watch_terminal".into(),
                format!(
                    "tmux display-message: {}",
                    String::from_utf8_lossy(&out.stderr)
                ),
            ));
        }
        let cmd = String::from_utf8_lossy(&out.stdout).trim().to_string();
        // Empty pane command or the configured default-shell means done.
        Ok(cmd)
    }

    /// `run_in_terminal`: send the command into the session, return the
    /// transcript so far. Follow up with `read_terminal`/`watch_terminal`.
    pub async fn run(&self, command: &str) -> ToolOutput {
        // Create the session lazily; `send-keys` fails without one.
        let init = self
            .tmux(&["new-session", "-d", "-s", &self.session])
            .kill_on_drop(true)
            .output()
            .await
            .map_err(|e| ToolError::Failed("run_in_terminal".into(), e.to_string()))?;
        // "duplicate session" is fine — the pane already exists.
        if !init.status.success()
            && !String::from_utf8_lossy(&init.stderr).contains("duplicate session")
        {
            return Err(ToolError::Failed(
                "run_in_terminal".into(),
                format!(
                    "tmux new-session: {}",
                    String::from_utf8_lossy(&init.stderr)
                ),
            ));
        }

        let payload = format!("{command}\n");
        let out = timeout(
            self.call_timeout,
            self.tmux(&["send-keys", "-t", &self.session, &payload])
                .kill_on_drop(true)
                .output(),
        )
        .await
        .map_err(|_| ToolError::Timeout("run_in_terminal".into(), self.call_timeout.as_secs()))?
        .map_err(|e| ToolError::Failed("run_in_terminal".into(), e.to_string()))?;
        if !out.status.success() {
            return Err(ToolError::Failed(
                "run_in_terminal".into(),
                format!("tmux send-keys: {}", String::from_utf8_lossy(&out.stderr)),
            ));
        }
        // The pane may need a moment to produce output.
        tokio::time::sleep(Duration::from_millis(300)).await;
        self.capture().await
    }

    /// `read_terminal`: the current pane transcript.
    pub async fn read(&self) -> ToolOutput {
        self.capture().await
    }

    /// `watch_terminal`: wait until the pane returns to the shell (the done
    /// signal), then capture. `max_secs` bounds the wait.
    pub async fn watch(&self, max_secs: u64) -> ToolOutput {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(max_secs.max(1));
        let poll = Duration::from_millis(400);
        loop {
            let current = self.command_running().await?;
            if !is_busy(&current) {
                break;
            }
            if tokio::time::Instant::now() + poll > deadline {
                return Err(ToolError::Timeout("watch_terminal".into(), max_secs.max(1)));
            }
            tokio::time::sleep(poll).await;
        }
        self.capture().await
    }

    async fn capture(&self) -> ToolOutput {
        let out = timeout(
            self.call_timeout,
            self.tmux(&["capture-pane", "-p", "-t", &self.session])
                .kill_on_drop(true)
                .output(),
        )
        .await
        .map_err(|_| ToolError::Timeout("read_terminal".into(), self.call_timeout.as_secs()))?
        .map_err(|e| ToolError::Failed("read_terminal".into(), e.to_string()))?;
        if !out.status.success() {
            // No pane: the session died between calls. Report honestly.
            return Err(ToolError::Failed(
                "read_terminal".into(),
                format!(
                    "tmux capture-pane: {}",
                    String::from_utf8_lossy(&out.stderr)
                ),
            ));
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_string())
    }
}

/// The shell-reappearing done signal: a busy pane shows the running command
/// (`htop`, `python3`, …). Shells we recognise: empty output (fresh pane)
/// or the obvious shell names.
fn is_busy(current_command: &str) -> bool {
    let c = current_command.trim();
    if c.is_empty() {
        return false;
    }
    !matches!(
        c,
        "bash" | "sh" | "zsh" | "fish" | "dash" | "ksh" | "zsh-5.9"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_names_mean_done() {
        assert!(!is_busy(""));
        assert!(!is_busy("bash"));
        assert!(!is_busy("zsh"));
        assert!(is_busy("htop"));
        assert!(is_busy("python3"));
        assert!(is_busy("tail"));
    }
}
