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

pub mod locate;
pub mod models;
pub mod profile;
pub mod secret;

/// The whole config file. Every field has a default; the file written on
/// first run shows each default commented out, so users can opt in per line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Reasoning provider name — never a key: openai, anthropic,
    /// openrouter, opencode-go, ollama, zai or local (no key). Key storage is the Secret
    /// Service (`cosmo auth-login`, one key per provider); env var
    /// `COSMO_API_KEY` for dev/CI only.
    pub provider: String,
    /// Model for the reasoning path. Empty is the provider's default.
    pub model: String,
    /// The provider's endpoint, as a whole URL. Empty is the provider's
    /// own; set it for a local server or a provider cosmo doesn't list.
    pub api_base: String,
    /// The request format: "openai" (chat completions) or "anthropic"
    /// (Messages). Empty is the provider's own.
    pub api_format: String,
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
    /// Speech-recognition models (phase 3), as directory names under
    /// `~/.cache/cosmo/models/asr/`. Empty is cosmo's default; `"none"`
    /// turns that model off (at least one must stay on). Streaming gives
    /// the live partials; offline gives the text that commits.
    pub asr_streaming_model: String,
    pub asr_offline_model: String,
    /// Threads for the streaming model. Nobody waits on a partial.
    pub asr_threads: u8,
    /// Threads for the offline model, 1–4: past four it stops getting
    /// faster (blueprint §3.3).
    pub offline_threads: u8,
    /// Barge-in (phase 5): keep the mic open while cosmo speaks. Off by
    /// default: on speakers cosmo hears its own voice (blueprint §8). Only
    /// for headsets; `doctor` warns while it's on.
    pub barge_in: bool,
    /// The wake word (phase 7): listen for `wake_phrase` while idle. Off by
    /// default: it's the user's choice to have a listening room. Local
    /// only; nothing leaves the machine unless a command needs the model.
    pub wake_word: bool,
    /// What wakes it ("cosmo", also after "hey"/"ok").
    pub wake_phrase: String,
    /// Log filter string, e.g. `cosmo=debug`. Overridden by `RUST_LOG`.
    pub log_filter: String,
    /// Where `web_search` looks things up: "wikipedia" (no key), or
    /// "ollama" / "tavily" (a key in the Secret Service, `cosmo search use`).
    pub search_provider: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            provider: "openai".into(),
            model: String::new(),
            api_base: String::new(),
            api_format: String::new(),
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
            asr_streaming_model: String::new(),
            asr_offline_model: String::new(),
            asr_threads: 2,
            offline_threads: 4,
            barge_in: false,
            wake_word: false,
            wake_phrase: "cosmo".into(),
            log_filter: "info".into(),
            search_provider: "wikipedia".into(),
        }
    }
}

/// The agent tools cosmo will accept (plan §1.3), as computer-use-linux
/// 0.5.0 names them (checked 2026-10-05 against its `tools/list`).
/// Deliberately absent: `run_shell` (invariant #2); `focused_window`
/// (broken on COSMIC, invariant #9); `setup_accessibility` and
/// `setup_window_targeting` (they change system settings); `doctor`.
pub fn default_allowed_tools() -> Vec<String> {
    [
        // Reading: windows, apps, and the screen as text (AT-SPI).
        "list_windows",
        "list_apps",
        "get_app_state",
        "screenshot",
        // Windows.
        "activate_window",
        "move_window",
        "resize_window",
        // Acting on what's on screen.
        "click",
        "scroll",
        "drag",
        "type_text",
        "press_key",
        "perform_action",
        "set_value",
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
        if !matches!(self.api_format.as_str(), "" | "openai" | "anthropic") {
            return Err(ConfigError::Parse(format!(
                "api_format {:?} must be \"openai\", \"anthropic\" or empty",
                self.api_format
            )));
        }
        let base = &self.api_base;
        if !(base.is_empty() || base.starts_with("https://") || base.starts_with("http://")) {
            return Err(ConfigError::Parse(format!(
                "api_base {:?} must be a whole http(s) URL",
                self.api_base
            )));
        }
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
        if !(1..=4).contains(&self.offline_threads) {
            return Err(ConfigError::Parse(format!(
                "offline_threads {} is out of range (1–4; more stops helping)",
                self.offline_threads
            )));
        }
        if self.asr_threads == 0 {
            return Err(ConfigError::Parse("asr_threads must be at least 1".into()));
        }
        if self.asr_streaming_model == "none" && self.asr_offline_model == "none" {
            return Err(ConfigError::Parse(
                "asr_streaming_model and asr_offline_model are both \"none\" — nothing would transcribe".into(),
            ));
        }
        if self.wake_phrase.trim().is_empty()
            || !self
                .wake_phrase
                .chars()
                .all(|c| c.is_alphabetic() || c == ' ' || c == '\'' || c == '-')
        {
            return Err(ConfigError::Parse(format!(
                "wake_phrase {:?} must be one or more plain words (letters only)",
                self.wake_phrase
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
// bug reports. Keys live in the Secret Service (`cosmo auth-login`); the
// env var COSMO_API_KEY is a dev/CI convenience only.

(
    // Reasoning provider name. The key itself is NOT configured here.
    // One of: openai, anthropic, openrouter, opencode-go, ollama, zai, or
    // local (a model on this computer, through Ollama, LM Studio or
    // llama.cpp: no key, nothing leaves the machine).
    // `cosmo auth-login` connects whichever is set: openrouter signs in with
    // your browser; the others take a pasted API key.
    // provider: "{provider}",
    // An empty model is the provider's default (openai: gpt-4o-mini,
    // anthropic: claude-haiku-4-5, openrouter: anthropic/claude-haiku-4.5,
    // opencode-go and zai: glm-5.3-flash, ollama: gpt-oss:120b,
    // local: granite4.1:8b).
    // model: "{model}",
    // A whole endpoint URL, for a local server or an unlisted provider.
    // api_base: "{api_base}",
    // "openai" (chat completions) or "anthropic" (Messages). OpenCode Go
    // serves its Qwen and MiniMax models in the "anthropic" format.
    // api_format: "{api_format}",

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
    // "kokoro" speaks locally once `cosmo models fetch` has run; "openai"
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

    // Speech recognition: model directory names under
    // ~/.cache/cosmo/models/asr/ (`cosmo models fetch`). Empty is
    // the default pair; "none" turns one off. Streaming shows live
    // partials; offline produces the text that commits.
    // asr_streaming_model: "{asr_streaming_model}",
    // asr_offline_model: "{asr_offline_model}",
    // asr_threads: {asr_threads},
    // offline_threads: {offline_threads},

    // Keep the mic open while cosmo speaks (barge-in). Headsets only: on
    // speakers cosmo would hear, and transcribe, its own voice.
    // barge_in: {barge_in},

    // Wake word: say "cosmo" (or "hey cosmo") to start a command, no key.
    // Recognised on this machine only. Off until you turn it on.
    // wake_word: {wake_word},
    // wake_phrase: "{wake_phrase}",

    // Log filter, e.g. "cosmo=debug". RUST_LOG wins when set.
    // log_filter: "{log_filter}",

    // Looking things up ("when does Dune 3 come out?"): "wikipedia" needs
    // no key; "ollama" (a free Ollama account) and "tavily" search the
    // whole web with a key (`cosmo search use ollama`).
    // search_provider: "{search_provider}",
)
"#,
        provider = d.provider,
        model = d.model,
        api_base = d.api_base,
        api_format = d.api_format,
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
        asr_streaming_model = d.asr_streaming_model,
        asr_offline_model = d.asr_offline_model,
        asr_threads = d.asr_threads,
        offline_threads = d.offline_threads,
        barge_in = d.barge_in,
        wake_word = d.wake_word,
        wake_phrase = d.wake_phrase,
        log_filter = d.log_filter,
        search_provider = d.search_provider,
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
        assert!(text.contains("// model: \"\","));
        assert!(text.contains("// api_base: \"\","));
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
    fn asr_settings_are_validated() {
        let d = Config::default();
        for bad in [0, 5] {
            let c = Config {
                offline_threads: bad,
                ..d.clone()
            };
            assert!(c.validate().is_err(), "offline_threads {bad}");
        }
        let c = Config {
            asr_threads: 0,
            ..d.clone()
        };
        assert!(c.validate().is_err());
        let off = Config {
            asr_streaming_model: "none".into(),
            ..d.clone()
        };
        assert!(off.validate().is_ok(), "one model is enough");
        let both = Config {
            asr_offline_model: "none".into(),
            ..off
        };
        assert!(both.validate().is_err());
        assert!(commented_default().contains("// offline_threads: 4,"));
    }

    #[test]
    fn run_shell_never_passes_validation() {
        let mut cfg = Config::default();
        cfg.allowed_tools.push("run_shell".into());
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn the_endpoint_overrides_are_checked() {
        let ok = Config {
            api_base: "http://localhost:11434/v1/chat/completions".into(),
            api_format: "anthropic".into(),
            ..Config::default()
        };
        assert!(ok.validate().is_ok());
        for (base, format) in [("localhost:11434", ""), ("", "grpc")] {
            let bad = Config {
                api_base: base.into(),
                api_format: format.into(),
                ..Config::default()
            };
            assert!(bad.validate().is_err(), "{base:?} {format:?}");
        }
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
