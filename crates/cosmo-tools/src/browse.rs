//! `open_url`: open a web address in the user's default browser, so "search
//! Google for…" needs no clicking or typing (the reasoning model builds the
//! search URL).
//!
//! Only `http` and `https`, and the URL is always one argument of its own:
//! never a shell, never a flag (it starts with the scheme). With
//! `new_window`, the browser's own `[Desktop Action new-window]` is used,
//! so the window opens on the current workspace instead of as a tab in a
//! window that may be on another one.

use std::process::{Command, Stdio};

use crate::{ToolError, ToolOutput};

fn failed(msg: impl Into<String>) -> ToolError {
    ToolError::Failed("open_url".into(), msg.into())
}

/// Accept a plain web address, nothing else.
pub fn check_url(url: &str) -> Result<&str, String> {
    let url = url.trim();
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .ok_or("only http:// and https:// addresses can be opened")?;
    if rest.is_empty() || rest.starts_with('/') {
        return Err("the address has no host".into());
    }
    if url.len() > 4096 {
        return Err("the address is too long".into());
    }
    if url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err("the address contains spaces or control characters: encode them".into());
    }
    Ok(url)
}

/// The default browser's `.desktop` id (`firefox`, `com.microsoft.Edge`).
fn default_browser() -> Option<String> {
    let out = Command::new("xdg-settings")
        .args(["get", "default-web-browser"])
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let id = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    let id = id.strip_suffix(".desktop").unwrap_or(&id).to_owned();
    (!id.is_empty()).then_some(id)
}

/// The argv that opens `url` in a new window of browser `id`: its
/// `new-window` action, with the URL in place of `%u` (or appended), and
/// `--new-window` added when the action's line lacks it (Chromium-family
/// entries rely on it being implied). `None` when there's no such action.
fn new_window_argv(id: &str, name: &str, url: &str) -> Option<Vec<String>> {
    let exec = cosmo_stt::hotwords::desktop_action_exec(id, "new-window")?;
    new_window_argv_from(&exec, name, url)
}

fn new_window_argv_from(exec: &str, name: &str, url: &str) -> Option<Vec<String>> {
    let mut argv = crate::launch::split_exec_url(exec, name, Some(url)).ok()?;
    if !argv.iter().any(|a| a == "--new-window") {
        let at = argv.iter().position(|a| a == url).unwrap_or(argv.len());
        argv.insert(at, "--new-window".into());
    }
    Some(argv)
}

/// Open `url` in the default browser; `new_window` asks for a fresh window
/// (on the current workspace).
pub fn open_url(url: &str, new_window: bool) -> ToolOutput {
    let url = check_url(url).map_err(failed)?;
    if new_window
        && let Some(id) = default_browser()
        && let Some(app) = cosmo_stt::hotwords::desktop_app(&id)
        && let Some(argv) = new_window_argv(&id, &app.name, url)
    {
        crate::launch::spawn_detached(&argv, app.path.as_deref())
            .map_err(|e| failed(format!("{}: {e}", app.name)))?;
        return Ok(format!("opened {url} in a new {} window", app.name));
    }
    let argv = vec!["xdg-open".to_owned(), url.to_owned()];
    crate::launch::spawn_detached(&argv, None).map_err(|e| failed(format!("xdg-open: {e}")))?;
    Ok(format!("opened {url} in the default browser"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_plain_web_addresses_pass() {
        assert!(check_url("https://www.google.com/search?q=cosmic+desktop").is_ok());
        assert!(check_url("http://localhost:8080/").is_ok());
        for bad in [
            "file:///etc/passwd",
            "javascript:alert(1)",
            "--new-window",
            "https://",
            "https:///path",
            "https://a b.com",
            "https://a.com/\nrm",
        ] {
            assert!(check_url(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn new_window_lines_get_the_url_and_the_flag() {
        let url = "https://www.google.com/search?q=x";
        // Firefox: the action has both already.
        assert_eq!(
            new_window_argv_from("firefox --new-window %u", "Firefox", url).unwrap(),
            ["firefox", "--new-window", url]
        );
        // Edge (Flatpak): neither; the flag goes before the URL.
        assert_eq!(
            new_window_argv_from(
                "/usr/bin/flatpak run --branch=stable --arch=x86_64 --command=/app/bin/edge com.microsoft.Edge",
                "Edge",
                url
            )
            .unwrap(),
            [
                "/usr/bin/flatpak",
                "run",
                "--branch=stable",
                "--arch=x86_64",
                "--command=/app/bin/edge",
                "com.microsoft.Edge",
                "--new-window",
                url
            ]
        );
        // A URL with spaces encoded stays one argument.
        let argv = new_window_argv_from("chromium %U", "Chromium", url).unwrap();
        assert_eq!(argv, ["chromium", "--new-window", url]);
    }
}
