//! `config.ron`: loading, commented-default first-run write, validation.
//!
//! Written with commented defaults on first run (COSMIC convention).
//!
//! **The config never holds a credential.** It may name a *provider*
//! (`provider = "openai"`); the API key itself lives in the Secret Service
//! (see implementation-plan.md §1.4). This file is the thing users paste into
//! bug reports — keep it paste-safe.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub mod secret;

/// The whole config file. Every field has a default; the file written on
/// first run shows each default commented out, so users can opt in per line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Reasoning provider name — never a key. Key storage is the Secret
    /// Service (`cosmo auth login`), env var `OPENAI_API_KEY` for dev/CI only.
    pub provider: String,
    /// Model for the reasoning path (phase 1: plain chat completions).
    pub model: String,
    /// MCP agent command to spawn (stdio transport).
    pub agent_command: String,
    /// Arguments passed to the agent command.
    pub agent_args: Vec<String>,
    /// MCP tool names the host will register. `run_shell` must never appear
    /// here; the host rejects it regardless (invariant #2: no shell).
    pub allowed_tools: Vec<String>,
    /// tmux session the terminal tools operate in.
    pub tmux_session: String,
    /// Voice provider for spoken output (phase 2). A provider name only —
    /// never a credential (same rule as `provider` above).
    pub voice_provider: String,
    /// Which voice within the provider. `"default"` defers to the provider's
    /// own choice; real ids come from `cosmo voice list`.
    pub voice_id: String,
    /// TTS model the voice provider uses. Provider-specific; empty means the
    /// provider's own default (Kokoro: the `fp32` export, §2.5; OpenAI:
    /// `gpt-4o-mini-tts`, §2.4).
    pub voice_model: String,
    /// Speaking-style instruction for providers that take one (OpenAI's
    /// `instructions` field: affect/tone/pacing). Empty = omit from requests.
    /// A style preference, never a credential.
    pub voice_instructions: String,
    /// `announce` minimum spacing between notifications, seconds.
    pub announce_spacing_secs: u64,
    /// Hold-to-talk trigger: an evdev key code (phase 3). 97 is Right Ctrl.
    /// Must be a key that does nothing on its own — it is never grabbed, so
    /// its press and release still reach the desktop (phase-3 findings §3).
    pub trigger_key: u16,
    /// Log filter string, e.g. `cosmo=debug`. Overridden by `RUST_LOG`.
    pub log_filter: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            provider: "openai".into(),
            model: "gpt-4o-mini".into(),
            agent_command: "computer-use-linux".into(),
            agent_args: vec!["mcp".into()],
            allowed_tools: default_allowed_tools(),
            tmux_session: "cosmo".into(),
            // Local by default (§2.5): Kokoro needs no key and no network
            // once `scripts/fetch-models` has run; "openai" is the cloud
            // alternative with a stored key.
            voice_provider: "kokoro".into(),
            voice_id: "default".into(),
            voice_model: String::new(),
            voice_instructions: String::new(),
            announce_spacing_secs: 8,
            trigger_key: 97,
            log_filter: "info".into(),
        }
    }
}

/// The ~a dozen agent tools cosmo will accept (plan §1.3). `run_shell` is
/// deliberately absent.
pub fn default_allowed_tools() -> Vec<String> {
    [
        "list_windows",
        "focus_window",
        "activate_window",
        "move_window",
        "resize_window",
        "screenshot",
        "click",
        "double_click",
        "right_click",
        "type_text",
        "press_key",
        "scroll",
        "drag",
        "get_accessibility_tree",
    ]
    .into_iter()
    .map(String::from)
    .collect()
}

/// `~/.config/cosmo/config.ron`.
pub fn config_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from(".config"))
        .join("cosmo")
        .join("config.ron")
}

/// Load the config, writing commented defaults on first run.
pub fn load() -> Result<Config, ConfigError> {
    load_from(&config_path())
}

pub fn load_from(path: &std::path::Path) -> Result<Config, ConfigError> {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            let cfg: Config =
                ron::from_str(&text).map_err(|e| ConfigError::Parse(e.to_string()))?;
            cfg.validate()?;
            Ok(cfg)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| ConfigError::Io(format!("{}: {e}", parent.display())))?;
            }
            std::fs::write(path, commented_default())
                .map_err(|e| ConfigError::Io(format!("{}: {e}", path.display())))?;
            Ok(Config::default())
        }
        Err(e) => Err(ConfigError::Io(format!("{}: {e}", path.display()))),
    }
}

impl Config {
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.allowed_tools.iter().any(|t| t == "run_shell") {
            return Err(ConfigError::Parse(
                "allowed_tools contains run_shell — shell execution is not a cosmo tool (invariant #2)".into(),
            ));
        }
        if self.allowed_tools.is_empty() {
            return Err(ConfigError::Parse(
                "allowed_tools is empty — the agent would have no tools".into(),
            ));
        }
        // Codes from BTN_MISC (0x100) up are mouse/joystick buttons.
        if self.trigger_key == 0 || self.trigger_key >= 0x100 {
            return Err(ConfigError::Parse(format!(
                "trigger_key {} is not a keyboard key code (1–255; Right Ctrl is 97)",
                self.trigger_key
            )));
        }
        if self.announce_spacing_secs < 8 {
            return Err(ConfigError::Parse(
                "announce_spacing_secs must be >= 8 (plan §1.3: >=8s notification spacing)".into(),
            ));
        }
        Ok(())
    }
}

/// Set single-line string fields in the config file, keeping everything
/// else — the user's comments, the commented defaults, field order —
/// exactly as it was (`cosmo voice set`, spec §2.7).
///
/// Per key: an active `key: …,` line is rewritten in place; otherwise its
/// commented default (`// key: …,`) is uncommented with the new value;
/// otherwise the field is added before the closing `)`. The result must
/// parse and validate as a [`Config`] before anything is written, and the
/// write is tmp-then-rename, so a failure leaves the old file intact.
///
/// Only for string-valued fields whose value fits on one line. This is not
/// a way to store a credential: callers pass provider and voice names.
pub fn set_string_fields(
    path: &std::path::Path,
    fields: &[(&str, &str)],
) -> Result<Config, ConfigError> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => commented_default(),
        Err(e) => return Err(ConfigError::Io(format!("{}: {e}", path.display()))),
    };
    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
    for (key, value) in fields {
        let literal = ron::to_string(value).map_err(|e| ConfigError::Parse(e.to_string()))?;
        let active = |l: &str| l.trim_start().starts_with(&format!("{key}:"));
        let commented = |l: &str| {
            l.trim_start()
                .strip_prefix("//")
                .is_some_and(|rest| rest.trim_start().starts_with(&format!("{key}:")))
        };
        let indent = |l: &str| l[..l.len() - l.trim_start().len()].to_owned();
        if let Some(i) = lines.iter().position(|l| active(l)) {
            lines[i] = format!("{}{key}: {literal},", indent(&lines[i]));
        } else if let Some(i) = lines.iter().position(|l| commented(l)) {
            lines[i] = format!("{}{key}: {literal},", indent(&lines[i]));
        } else if let Some(i) = lines.iter().rposition(|l| l.trim() == ")") {
            lines.insert(i, format!("    {key}: {literal},"));
        } else {
            return Err(ConfigError::Parse(format!(
                "{}: no closing `)` to add `{key}` before",
                path.display()
            )));
        }
    }
    let mut new_text = lines.join("\n");
    new_text.push('\n');
    let cfg: Config = ron::from_str(&new_text).map_err(|e| ConfigError::Parse(e.to_string()))?;
    cfg.validate()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| ConfigError::Io(format!("{}: {e}", parent.display())))?;
    }
    let tmp = path.with_extension("ron.tmp");
    std::fs::write(&tmp, &new_text)
        .map_err(|e| ConfigError::Io(format!("{}: {e}", tmp.display())))?;
    std::fs::rename(&tmp, path).map_err(|e| ConfigError::Io(format!("{}: {e}", path.display())))?;
    Ok(cfg)
}

/// Errors surfaced by loading/validating the config.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("config parse: {0}")]
    Parse(String),
    #[error("config io: {0}")]
    Io(String),
}

/// The first-run file: a struct body where every default is commented out.
/// RON accepts the empty body and serde fills each field with its default;
/// uncomment a line (and the opening content) to override.
pub fn commented_default() -> String {
    let d = Config::default();
    let tools = d
        .allowed_tools
        .iter()
        .map(|t| format!("        // \"{t}\","))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        r#"// cosmo configuration (~/.config/cosmo/config.ron)
//
// Every field below is commented out and falls back to its default. To
// override a field, uncomment it (keep the surrounding `( ... )`).
//
// NOTE: never put an API key in this file. It is the file users paste into
// bug reports. Keys live in the Secret Service (`cosmo auth login`); the
// env var OPENAI_API_KEY is a dev/CI convenience only.

(
    // Reasoning provider name. The key itself is NOT configured here.
    // provider: "{provider}",
    // model: "{model}",

    // MCP agent (stdio transport).
    // agent_command: "{agent_command}",
    // agent_args: [{agent_args}],

    // Agent tools the host will register. `run_shell` is rejected
    // outright if listed — cosmo never hands the model a shell.
    // allowed_tools: [
{tools}
    // ],

    // tmux session used by the terminal tools.
    // tmux_session: "{tmux_session}",

    // Voice output (phase 2). A provider name only — never a key.
    // "kokoro" speaks locally once `scripts/fetch-models` has run; "openai"
    // is the cloud alternative (uses the stored key). Voice ids come from
    // `cosmo voice list`; "default" is the provider's pick. An empty
    // voice_model is the provider's default (kokoro: "fp32" | "fp16" | "q8").
    // voice_provider: "{voice_provider}",
    // voice_id: "{voice_id}",
    // voice_model: "{voice_model}",
    // Speaking style for providers that take one (OpenAI instructions).
    // voice_instructions: "{voice_instructions}",

    // Minimum spacing between `announce` notifications (seconds).
    // announce_spacing_secs: {spacing},

    // Hold-to-talk key, as an evdev key code. 97 = Right Ctrl. It is never
    // grabbed, so pick a key that does nothing when pressed on its own.
    // trigger_key: {trigger_key},

    // Log filter, e.g. "cosmo=debug". RUST_LOG wins when set.
    // log_filter: "{log_filter}",
)
"#,
        provider = d.provider,
        model = d.model,
        agent_command = d.agent_command,
        agent_args = d
            .agent_args
            .iter()
            .map(|a| format!("\"{a}\""))
            .collect::<Vec<_>>()
            .join(", "),
        tools = tools,
        tmux_session = d.tmux_session,
        voice_provider = d.voice_provider,
        voice_id = d.voice_id,
        voice_model = d.voice_model,
        voice_instructions = d.voice_instructions,
        spacing = d.announce_spacing_secs,
        trigger_key = d.trigger_key,
        log_filter = d.log_filter,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let cfg = Config::default();
        let text = ron::to_string(&cfg).unwrap();
        let back: Config = ron::from_str(&text).unwrap();
        assert_eq!(cfg, back);
    }

    #[test]
    fn first_run_writes_commented_defaults() {
        let dir = std::env::temp_dir().join(format!("cosmo-cfg-{}", std::process::id()));
        let path = dir.join("cosmo").join("config.ron");
        let _ = std::fs::remove_dir_all(&dir);
        let cfg = load_from(&path).unwrap();
        assert_eq!(cfg, Config::default());
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("// model: \"gpt-4o-mini\","));
        assert!(text.contains("// voice_provider: \"kokoro\","));
        assert!(text.contains("// voice_id: \"default\","));
        assert!(text.contains("// voice_model: \"\","));
        // Second load re-reads the (all-commented) file back to defaults.
        let again = load_from(&path).unwrap();
        assert_eq!(again, Config::default());
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("cosmo-cfg-set-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("config.ron")
    }

    /// The first-run file: the commented default is uncommented in place;
    /// every other line — comments included — is untouched.
    #[test]
    fn set_uncomments_the_default_and_keeps_everything_else() {
        let path = scratch("uncomment");
        std::fs::write(&path, commented_default()).unwrap();
        let before = std::fs::read_to_string(&path).unwrap();
        let cfg = set_string_fields(
            &path,
            &[("voice_provider", "kokoro"), ("voice_id", "bm_george")],
        )
        .unwrap();
        assert_eq!(cfg.voice_id, "bm_george");
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(after.contains("    voice_id: \"bm_george\","));
        assert!(!after.contains("// voice_id:"));
        let changed = before
            .lines()
            .zip(after.lines())
            .filter(|(a, b)| a != b)
            .count();
        assert_eq!(changed, 2, "only the two voice lines change");
        assert_eq!(before.lines().count(), after.lines().count());
        assert_eq!(load_from(&path).unwrap().voice_id, "bm_george");
    }

    /// A value the user already set is rewritten, not duplicated; a field
    /// missing entirely is added before the closing paren.
    #[test]
    fn set_rewrites_active_lines_and_adds_missing_ones() {
        let path = scratch("rewrite");
        std::fs::write(
            &path,
            "// mine\n(\n    voice_id: \"af_bella\", // my pick\n)\n",
        )
        .unwrap();
        set_string_fields(
            &path,
            &[("voice_id", "bm_lewis"), ("voice_provider", "kokoro")],
        )
        .unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text.matches("voice_id:").count(), 1);
        assert!(text.starts_with("// mine\n"));
        let cfg = load_from(&path).unwrap();
        assert_eq!(
            (cfg.voice_id.as_str(), cfg.voice_provider.as_str()),
            ("bm_lewis", "kokoro")
        );
    }

    /// Quotes and backslashes are escaped by RON itself, and a file the
    /// edit would break is left exactly as it was.
    #[test]
    fn set_escapes_values_and_never_writes_a_broken_file() {
        let path = scratch("escape");
        std::fs::write(&path, commented_default()).unwrap();
        set_string_fields(&path, &[("voice_id", "we\"ird\\id")]).unwrap();
        assert_eq!(load_from(&path).unwrap().voice_id, "we\"ird\\id");

        let broken = "(\n    announce_spacing_secs: 2,\n)\n";
        std::fs::write(&path, broken).unwrap();
        assert!(set_string_fields(&path, &[("voice_id", "x")]).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), broken);
    }

    #[test]
    fn trigger_key_must_be_a_keyboard_key() {
        let ok = Config::default();
        assert_eq!(ok.trigger_key, 97);
        assert!(ok.validate().is_ok());
        for bad in [0u16, 0x100, 0x110] {
            let cfg = Config {
                trigger_key: bad,
                ..Config::default()
            };
            assert!(cfg.validate().is_err(), "{bad} accepted");
        }
        assert!(commented_default().contains("// trigger_key: 97,"));
    }

    #[test]
    fn run_shell_never_passes_validation() {
        let mut cfg = Config::default();
        cfg.allowed_tools.push("run_shell".into());
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn malformed_file_is_an_error() {
        let dir = std::env::temp_dir().join(format!("cosmo-cfg-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.ron");
        std::fs::write(&path, "this is not ron").unwrap();
        assert!(matches!(load_from(&path), Err(ConfigError::Parse(_))));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
