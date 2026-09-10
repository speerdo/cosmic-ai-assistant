//! `remember`: flat file, the only memory spanning two sittings
//! (blueprint §6). One line per entry keeps the file grep-able and
//! prompt-sliceable.

use std::path::PathBuf;

use crate::{ToolError, ToolOutput};

/// Location: `$XDG_STATE_HOME/cosmo/remember.txt` (state, not cache: the
/// user would reasonably back it up).
pub fn memory_path() -> PathBuf {
    let base = std::env::var("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
            PathBuf::from(home).join(".local/state")
        });
    base.join("cosmo").join("remember.txt")
}

/// Append one line.
pub async fn add(line: &str) -> ToolOutput {
    let line = line.trim();
    if line.is_empty() {
        return Err(ToolError::Failed("remember".into(), "empty line".into()));
    }
    if line.contains('\n') {
        return Err(ToolError::Failed(
            "remember".into(),
            "one line per entry — no embedded newlines".into(),
        ));
    }
    let path = memory_path();
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| ToolError::Failed("remember".into(), e.to_string()))?;
    }
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .await
        .map_err(|e| ToolError::Failed("remember".into(), e.to_string()))?;
    use tokio::io::AsyncWriteExt;
    file.write_all(format!("{line}\n").as_bytes())
        .await
        .map_err(|e| ToolError::Failed("remember".into(), e.to_string()))?;
    Ok(format!("remembered: {line}"))
}

/// Read the whole file (small by design; phase 5 slices it into the prompt
/// within budget).
pub async fn read() -> ToolOutput {
    match tokio::fs::read_to_string(memory_path()).await {
        Ok(text) => Ok(if text.trim().is_empty() {
            "(nothing remembered yet)".into()
        } else {
            text
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok("(nothing remembered yet)".into()),
        Err(e) => Err(ToolError::Failed("remember".into(), e.to_string())),
    }
}
