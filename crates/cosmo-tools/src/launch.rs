//! `launch_app`: start an installed application from its `.desktop` entry
//! (phase-4 spec §4.4). A reflex verb, and a tool the reasoning path can
//! call.
//!
//! **No shell, ever.** `Exec=` is split into argv by the Desktop Entry
//! Specification's own rules (quoting, escapes, field codes) and started
//! directly, so a strange name or `Exec` line can't turn into a command
//! line. Only installed, visible applications can be named, by id; apps that
//! need a terminal (`Terminal=true`) are refused: the reasoning path owns
//! terminals.

use std::process::{Command, Stdio};

use crate::{ToolError, ToolOutput};

fn failed(msg: impl Into<String>) -> ToolError {
    ToolError::Failed("launch_app".into(), msg.into())
}

/// Split an `Exec=` value into argv, per the Desktop Entry Specification
/// ("The Exec key"): double-quoted arguments with `\` escaping `"`, `` ` ``,
/// `$` and `\` inside them; field codes removed (cosmo never passes files or
/// URLs), `%c` replaced with the app's name, `%%` a literal `%`. An
/// unterminated quote or a stray escape is an error, not a guess.
pub fn split_exec(exec: &str, name: &str) -> Result<Vec<String>, String> {
    let mut args: Vec<String> = Vec::new();
    let mut arg = String::new();
    let mut started = false; // an empty quoted arg ("") is still an arg
    let mut chars = exec.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                started = true;
                loop {
                    match chars.next() {
                        None => return Err("unterminated quote in Exec".into()),
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some(e @ ('"' | '`' | '$' | '\\')) => arg.push(e),
                            _ => return Err("invalid escape in a quoted Exec argument".into()),
                        },
                        Some(c) => arg.push(c),
                    }
                }
            }
            ' ' | '\t' => {
                if started || !arg.is_empty() {
                    args.push(std::mem::take(&mut arg));
                    started = false;
                }
            }
            '%' => match chars.next() {
                Some('%') => arg.push('%'),
                Some('c') => arg.push_str(name),
                // Files, URLs, icon, desktop file, deprecated codes: dropped.
                Some('f' | 'F' | 'u' | 'U' | 'i' | 'k' | 'd' | 'D' | 'n' | 'N' | 'v' | 'm') => {
                    started = started || !arg.is_empty();
                }
                _ => return Err("invalid field code in Exec".into()),
            },
            c => arg.push(c),
        }
    }
    if started || !arg.is_empty() {
        args.push(arg);
    }
    // A field code alone leaves an empty argument behind; drop those.
    args.retain(|a| !a.is_empty());
    if args.is_empty() {
        return Err("empty Exec".into());
    }
    Ok(args)
}

/// Start the application with `.desktop` id `id`, detached: it outlives
/// cosmo, and a signal to cosmo's process group doesn't reach it.
pub fn launch(id: &str) -> ToolOutput {
    let app = cosmo_stt::hotwords::desktop_app(id)
        .ok_or_else(|| failed(format!("no installed application `{id}`")))?;
    if app.terminal {
        return Err(failed(format!("{} runs in a terminal", app.name)));
    }
    let exec = app
        .exec
        .as_deref()
        .ok_or_else(|| failed(format!("{} has no Exec line", app.name)))?;
    let argv = split_exec(exec, &app.name).map_err(|e| failed(format!("{}: {e}", app.name)))?;
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(dir) = &app.path {
        cmd.current_dir(dir);
    }
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| failed(format!("{}: {e}", app.name)))?;
    // Reap it when it exits, so it never lingers as a zombie of cosmo's.
    std::thread::Builder::new()
        .name("cosmo-launch-reap".into())
        .spawn(move || {
            let _ = child.wait();
        })
        .map_err(|e| failed(e.to_string()))?;
    Ok(format!("started {}", app.name))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn split(e: &str) -> Vec<String> {
        split_exec(e, "Name").unwrap()
    }

    #[test]
    fn plain_exec_lines_split_on_spaces_and_drop_field_codes() {
        assert_eq!(split("firefox %u"), ["firefox"]);
        assert_eq!(split("thunderbird %u"), ["thunderbird"]);
        assert_eq!(
            split("/usr/bin/codium --unity-launch %F"),
            ["/usr/bin/codium", "--unity-launch"]
        );
        assert_eq!(
            split(
                "/usr/bin/flatpak run --branch=stable --arch=x86_64 --command=dbeaver io.dbeaver.DBeaverCommunity"
            ),
            [
                "/usr/bin/flatpak",
                "run",
                "--branch=stable",
                "--arch=x86_64",
                "--command=dbeaver",
                "io.dbeaver.DBeaverCommunity"
            ]
        );
    }

    #[test]
    fn quoting_and_escapes_follow_the_spec() {
        assert_eq!(
            split(r#""/opt/My App/run" --flag"#),
            ["/opt/My App/run", "--flag"]
        );
        assert_eq!(
            split(r#"sh-free "a \"quoted\" \$HOME \`x\` \\ b""#),
            ["sh-free", r#"a "quoted" $HOME `x` \ b"#]
        );
        assert_eq!(
            split("app --title=%c 100%%"),
            ["app", "--title=Name", "100%"]
        );
        assert_eq!(
            split(r#"app """#),
            ["app"],
            "an empty quoted arg is dropped as empty"
        );
    }

    #[test]
    fn malformed_exec_lines_are_refused_not_guessed() {
        assert!(split_exec(r#""unterminated"#, "n").is_err());
        assert!(split_exec(r#""bad \q escape""#, "n").is_err());
        assert!(split_exec("app %z", "n").is_err());
        assert!(split_exec("   %U  ", "n").is_err(), "nothing left to run");
    }

    /// Shell syntax in an Exec line is just text: nothing interprets it.
    #[test]
    fn shell_syntax_is_never_interpreted() {
        assert_eq!(split("app ; rm -rf ~"), ["app", ";", "rm", "-rf", "~"]);
        assert_eq!(
            split("app $(whoami) | cat"),
            ["app", "$(whoami)", "|", "cat"]
        );
    }

    #[test]
    fn unknown_apps_are_refused() {
        assert!(launch("definitely-not-installed-cosmo-test").is_err());
    }
}
