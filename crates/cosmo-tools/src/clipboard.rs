//! Clipboard tools: `clipboard_get` / `clipboard_set` (plan §1.3).
//!
//! `wl-clipboard-rs` talks the Wayland data-control protocol directly — no
//! `wl-copy` subprocess, no shell. Both tools are gated:
//!
//! - **`clipboard_get` holds.** Whatever the user last copied — a password, a
//!   token, a private message — becomes model context the moment it is read,
//!   and it leaves the machine. That is a decision for the user to make per
//!   call, so [`cosmo_gate::Verdict::Hold`] parks it for a local confirm.
//! - **Both are lock-sensitive** (invariant #10): the clipboard is a shared
//!   surface belonging to the user's session, so reads *and* writes refuse
//!   while the screen is locked or lock state is unknown. On COSMIC that
//!   means they refuse outright today (findings §L) — correct, not a bug.
//!
//! Neither check lives here. The gate owns both; this module is the
//! mechanism, and it is reached only after a verdict.

use std::io::Read;

use crate::{ToolError, ToolOutput};

/// Longest clipboard payload handed to the model. A clipboard can hold a
/// whole file; the reasoning loop's token budget (invariant #7) cannot.
const MAX_BYTES: usize = 8 * 1024;

fn failed(tool: &str, e: impl std::fmt::Display) -> ToolError {
    ToolError::Failed(tool.into(), e.to_string())
}

/// Read the clipboard as UTF-8 text.
///
/// An empty clipboard is a normal answer, not an error — the model should be
/// told "it is empty" rather than handed a failure to reason about.
pub async fn get() -> ToolOutput {
    tokio::task::spawn_blocking(|| -> ToolOutput {
        use wl_clipboard_rs::paste::{ClipboardType, Error, MimeType, Seat, get_contents};

        match get_contents(ClipboardType::Regular, Seat::Unspecified, MimeType::Text) {
            Ok((mut pipe, _mime)) => {
                let mut buf = Vec::new();
                pipe.read_to_end(&mut buf)
                    .map_err(|e| failed("clipboard_get", e))?;
                let truncated = buf.len() > MAX_BYTES;
                if truncated {
                    buf.truncate(MAX_BYTES);
                }
                let mut text = String::from_utf8_lossy(&buf).into_owned();
                if truncated {
                    text.push_str("\n… (clipboard truncated)");
                }
                Ok(text)
            }
            Err(Error::ClipboardEmpty | Error::NoMimeType) => {
                Ok("the clipboard is empty".to_owned())
            }
            Err(e) => Err(failed("clipboard_get", e)),
        }
    })
    .await
    .map_err(|e| failed("clipboard_get", e))?
}

/// Replace the clipboard contents with `text`.
pub async fn set(text: &str) -> ToolOutput {
    let text = text.to_owned();
    tokio::task::spawn_blocking(move || -> ToolOutput {
        use wl_clipboard_rs::copy::{MimeType, Options, Source};

        let bytes = text.clone().into_bytes();
        let mut opts = Options::new();
        // Serve from a forked child so the daemon's own loop is not blocked
        // holding the selection for the rest of the session.
        opts.foreground(false);
        opts.copy(Source::Bytes(bytes.into()), MimeType::Autodetect)
            .map_err(|e| failed("clipboard_set", e))?;
        Ok(format!(
            "copied {} characters to the clipboard",
            text.chars().count()
        ))
    })
    .await
    .map_err(|e| failed("clipboard_set", e))?
}

#[cfg(test)]
mod tests {
    /// The gate, not this module, decides whether these run — but the
    /// registry and the gate must agree about their names, or the tools are
    /// registered under names no gate arm matches.
    #[test]
    fn registry_names_match_the_gate() {
        for name in ["clipboard_get", "clipboard_set"] {
            assert!(
                crate::registry::NAMES.contains(&name),
                "{name} missing from the native registry"
            );
            assert!(
                cosmo_gate::is_lock_sensitive(name),
                "{name} must be lock-sensitive (invariant #10)"
            );
        }
    }
}
