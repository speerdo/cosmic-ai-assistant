//! Which service the reasoning path talks to, and in which format.
//!
//! cosmo speaks two wire formats: OpenAI's chat completions (most providers
//! copy it) and Anthropic's Messages API (`anthropic.rs`). A provider is a
//! name in `config.ron` (`provider`), which picks the endpoint, the format,
//! how the key is sent, where `cosmo auth login` sends you for a key, and
//! the model used when `model` is left empty. `api_base` and `api_format`
//! override the endpoint and format for anything not in the table.
//!
//! Endpoints were checked against each provider's docs on 2026-10-02.

use cosmo_config::Config;

/// The request format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// `POST …/chat/completions`, OpenAI's shape.
    OpenAiChat,
    /// `POST …/v1/messages`, Anthropic's shape.
    AnthropicMessages,
}

impl Format {
    /// The `api_format` config value.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "openai" => Some(Self::OpenAiChat),
            "anthropic" => Some(Self::AnthropicMessages),
            _ => None,
        }
    }

    /// The path `COSMO_API_BASE` (tests, local servers) is joined with.
    fn path(self) -> &'static str {
        match self {
            Self::OpenAiChat => "/v1/chat/completions",
            Self::AnthropicMessages => "/v1/messages",
        }
    }
}

/// How the key travels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Auth {
    /// `Authorization: Bearer <key>`.
    Bearer,
    /// `x-api-key: <key>` (Anthropic's own API).
    XApiKey,
}

/// One row of the table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Preset {
    pub name: &'static str,
    /// The endpoint for each format the provider serves.
    pub chat_url: Option<&'static str>,
    pub messages_url: Option<&'static str>,
    /// The format used when `api_format` is empty.
    pub format: Format,
    /// How the key is sent to the Messages endpoint (chat is always Bearer).
    pub messages_auth: Auth,
    /// Where `cosmo auth login` opens.
    pub key_page: &'static str,
    /// The model used when `model` is empty.
    pub default_model: &'static str,
    /// A caution `cosmo auth login` and `doctor` show (terms of use).
    pub note: Option<&'static str>,
}

/// Every provider cosmo knows by name.
pub const PRESETS: &[Preset] = &[
    Preset {
        name: "openai",
        chat_url: Some("https://api.openai.com/v1/chat/completions"),
        messages_url: None,
        format: Format::OpenAiChat,
        messages_auth: Auth::Bearer,
        key_page: "https://platform.openai.com/api-keys",
        default_model: "gpt-4o-mini",
        note: None,
    },
    Preset {
        name: "anthropic",
        chat_url: None,
        messages_url: Some("https://api.anthropic.com/v1/messages"),
        format: Format::AnthropicMessages,
        messages_auth: Auth::XApiKey,
        key_page: "https://platform.claude.com/settings/keys",
        default_model: "claude-haiku-4-5",
        note: Some(
            "needs an API key from the Claude Console; a Claude Pro/Max subscription is not one",
        ),
    },
    Preset {
        name: "openrouter",
        chat_url: Some("https://openrouter.ai/api/v1/chat/completions"),
        messages_url: None,
        format: Format::OpenAiChat,
        messages_auth: Auth::Bearer,
        key_page: "https://openrouter.ai/settings/keys",
        default_model: "anthropic/claude-haiku-4.5",
        note: None,
    },
    Preset {
        name: "opencode-go",
        chat_url: Some("https://opencode.ai/zen/go/v1/chat/completions"),
        messages_url: Some("https://opencode.ai/zen/go/v1/messages"),
        format: Format::OpenAiChat,
        messages_auth: Auth::Bearer,
        key_page: "https://opencode.ai/auth",
        default_model: "glm-5.3-flash",
        note: Some(
            "OpenCode Go's terms say it is designed for coding agents; voice-assistant \
             traffic may not be what they allow. Your account, your call",
        ),
    },
    Preset {
        name: "ollama",
        chat_url: Some("https://ollama.com/v1/chat/completions"),
        messages_url: None,
        format: Format::OpenAiChat,
        messages_auth: Auth::Bearer,
        key_page: "https://ollama.com/settings/keys",
        default_model: "gpt-oss:120b",
        note: None,
    },
    Preset {
        name: "zai",
        chat_url: Some("https://api.z.ai/api/paas/v4/chat/completions"),
        messages_url: None,
        format: Format::OpenAiChat,
        messages_auth: Auth::Bearer,
        key_page: "https://z.ai/manage-apikey/apikey-list",
        default_model: "glm-5.3-flash",
        note: None,
    },
];

/// Look a provider up by its config name.
pub fn preset(name: &str) -> Option<&'static Preset> {
    PRESETS.iter().find(|p| p.name == name)
}

/// The provider names, for error messages.
pub fn names() -> String {
    PRESETS
        .iter()
        .map(|p| p.name)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Everything one request needs to know, resolved from the config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    pub provider: &'static Preset,
    pub url: String,
    pub format: Format,
    pub auth: Auth,
    pub model: String,
}

impl Endpoint {
    /// Resolve `provider`, `api_format`, `api_base` and `model`.
    /// `env_base` is `COSMO_API_BASE` (tests and local servers): a base the
    /// format's standard path is appended to.
    pub fn resolve(cfg: &Config, env_base: Option<&str>) -> Result<Self, String> {
        let provider = preset(&cfg.provider)
            .ok_or_else(|| format!("unknown provider {:?} (known: {})", cfg.provider, names()))?;
        let format = if cfg.api_format.is_empty() {
            provider.format
        } else {
            Format::parse(&cfg.api_format).ok_or_else(|| {
                format!(
                    "api_format {:?} is not \"openai\" or \"anthropic\"",
                    cfg.api_format
                )
            })?
        };
        let url = if let Some(base) = env_base.filter(|b| !b.trim().is_empty()) {
            format!("{}{}", base.trim_end_matches('/'), format.path())
        } else if !cfg.api_base.is_empty() {
            cfg.api_base.clone()
        } else {
            let url = match format {
                Format::OpenAiChat => provider.chat_url,
                Format::AnthropicMessages => provider.messages_url,
            };
            url.ok_or_else(|| {
                format!(
                    "provider {:?} has no {} endpoint; set api_base",
                    provider.name, cfg.api_format
                )
            })?
            .to_owned()
        };
        let auth = match format {
            Format::OpenAiChat => Auth::Bearer,
            Format::AnthropicMessages => provider.messages_auth,
        };
        let model = if cfg.model.trim().is_empty() {
            provider.default_model.to_owned()
        } else {
            cfg.model.clone()
        };
        Ok(Self {
            provider,
            url,
            format,
            auth,
            model,
        })
    }
}

/// The `User-Agent` every request carries: OpenCode asks for a distinctive
/// one, and it's good manners everywhere.
pub fn user_agent() -> String {
    format!("cosmo/{}", env!("CARGO_PKG_VERSION"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(provider: &str) -> Config {
        Config {
            provider: provider.into(),
            ..Config::default()
        }
    }

    #[test]
    fn every_preset_resolves_with_its_own_default_model() {
        for p in PRESETS {
            let e = Endpoint::resolve(&cfg(p.name), None).unwrap();
            assert_eq!(e.model, p.default_model, "{}", p.name);
            assert!(e.url.starts_with("https://"), "{}", p.name);
            assert_eq!(e.format, p.format, "{}", p.name);
        }
    }

    #[test]
    fn the_formats_and_auth_follow_the_provider() {
        let a = Endpoint::resolve(&cfg("anthropic"), None).unwrap();
        assert_eq!(a.url, "https://api.anthropic.com/v1/messages");
        assert_eq!(
            (a.format, a.auth),
            (Format::AnthropicMessages, Auth::XApiKey)
        );

        let o = Endpoint::resolve(&cfg("openrouter"), None).unwrap();
        assert_eq!(o.url, "https://openrouter.ai/api/v1/chat/completions");
        assert_eq!(o.auth, Auth::Bearer);

        // Z.ai's path is /v4, not /v1: the table holds whole URLs.
        let z = Endpoint::resolve(&cfg("zai"), None).unwrap();
        assert!(z.url.ends_with("/api/paas/v4/chat/completions"));
    }

    #[test]
    fn opencode_go_serves_qwen_and_minimax_in_anthropic_format() {
        let c = Config {
            api_format: "anthropic".into(),
            model: "qwen3.8-plus".into(),
            ..cfg("opencode-go")
        };
        let e = Endpoint::resolve(&c, None).unwrap();
        assert_eq!(e.url, "https://opencode.ai/zen/go/v1/messages");
        assert_eq!(
            (e.format, e.auth),
            (Format::AnthropicMessages, Auth::Bearer)
        );
        assert_eq!(e.model, "qwen3.8-plus");
    }

    #[test]
    fn overrides_win() {
        let c = Config {
            api_base: "http://localhost:11434/v1/chat/completions".into(),
            model: "llama3.3".into(),
            ..cfg("ollama")
        };
        let e = Endpoint::resolve(&c, None).unwrap();
        assert_eq!(e.url, "http://localhost:11434/v1/chat/completions");
        // The env base (tests) beats both, with the format's path.
        let e = Endpoint::resolve(&c, Some("http://127.0.0.1:9/")).unwrap();
        assert_eq!(e.url, "http://127.0.0.1:9/v1/chat/completions");
        let e = Endpoint::resolve(&cfg("anthropic"), Some("http://127.0.0.1:9")).unwrap();
        assert_eq!(e.url, "http://127.0.0.1:9/v1/messages");
    }

    #[test]
    fn mistakes_say_what_is_known() {
        let err = Endpoint::resolve(&cfg("gemini"), None).unwrap_err();
        assert!(
            err.contains("openrouter") && err.contains("anthropic"),
            "{err}"
        );
        let c = Config {
            api_format: "anthropic".into(),
            ..cfg("zai")
        };
        assert!(
            Endpoint::resolve(&c, None)
                .unwrap_err()
                .contains("api_base")
        );
        let c = Config {
            api_format: "grpc".into(),
            ..cfg("openai")
        };
        assert!(Endpoint::resolve(&c, None).is_err());
    }
}
