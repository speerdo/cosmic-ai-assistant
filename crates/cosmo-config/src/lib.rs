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
    /// TTS model the voice provider uses (spec §2.4). Provider-specific;
    /// OpenAI's is `gpt-4o-mini-tts`.
    pub voice_model: String,
    /// Speaking-style instruction for providers that take one (OpenAI's
    /// `instructions` field: affect/tone/pacing). Empty = omit from requests.
    /// A style preference, never a credential.
    pub voice_instructions: String,
    /// `announce` minimum spacing between notifications, seconds.
    pub announce_spacing_secs: u64,
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
            // "kokoro" is the phase-2 end state once local models are
            // fetched (§2.5); "openai" works today with a stored key.
            voice_provider: "openai".into(),
            voice_id: "default".into(),
            voice_model: "gpt-4o-mini-tts".into(),
            voice_instructions: String::new(),
            announce_spacing_secs: 8,
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
        if self.announce_spacing_secs < 8 {
            return Err(ConfigError::Parse(
                "announce_spacing_secs must be >= 8 (plan §1.3: >=8s notification spacing)".into(),
            ));
        }
        Ok(())
    }
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
    // "kokoro" is the intended default once local models are fetched
    // (`scripts/fetch-models`); "openai" works today with a stored key.
    // voice_provider: "{voice_provider}",
    // voice_id: "{voice_id}",
    // voice_model: "{voice_model}",
    // Speaking style for providers that take one (OpenAI instructions).
    // voice_instructions: "{voice_instructions}",

    // Minimum spacing between `announce` notifications (seconds).
    // announce_spacing_secs: {spacing},

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
        assert!(text.contains("// voice_provider: \"openai\","));
        assert!(text.contains("// voice_id: \"default\","));
        assert!(text.contains("// voice_model: \"gpt-4o-mini-tts\","));
        // Second load re-reads the (all-commented) file back to defaults.
        let again = load_from(&path).unwrap();
        assert_eq!(again, Config::default());
        let _ = std::fs::remove_dir_all(&dir);
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
