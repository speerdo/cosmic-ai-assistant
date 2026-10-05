//! Which service the reasoning path talks to, and in which format.
//!
//! cosmo speaks two wire formats: OpenAI's chat completions (most providers
//! copy it) and Anthropic's Messages API (`anthropic.rs`). A provider is a
//! name in `config.ron` (`provider`), which picks the endpoint, the format,
//! how the key is sent, where `cosmo auth-login` sends you for a key, and
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
    /// How it's shown to people (the applet).
    pub label: &'static str,
    /// False for a local server: no key to store, and none is sent unless
    /// one was stored anyway (llama.cpp's `--api-key`).
    pub needs_key: bool,
    /// The endpoint for each format the provider serves.
    pub chat_url: Option<&'static str>,
    pub messages_url: Option<&'static str>,
    /// The format used when `api_format` is empty.
    pub format: Format,
    /// How the key is sent to the Messages endpoint (chat is always Bearer).
    pub messages_auth: Auth,
    /// Where `cosmo auth-login` opens to have a key pasted.
    pub key_page: &'static str,
    /// The provider's browser sign-in for third-party apps, when it has
    /// one: `cosmo auth-login` then needs no pasting (`login.rs`).
    pub browser_login: Option<crate::login::BrowserLogin>,
    /// The model used when `model` is empty.
    pub default_model: &'static str,
    /// A caution `cosmo auth-login` and `doctor` show (terms of use).
    pub note: Option<&'static str>,
}

/// Every provider cosmo knows by name.
pub const PRESETS: &[Preset] = &[
    Preset {
        name: "openai",
        label: "OpenAI",
        needs_key: true,
        chat_url: Some("https://api.openai.com/v1/chat/completions"),
        messages_url: None,
        format: Format::OpenAiChat,
        messages_auth: Auth::Bearer,
        key_page: "https://platform.openai.com/api-keys",
        browser_login: None,
        default_model: "gpt-4o-mini",
        note: None,
    },
    Preset {
        name: "anthropic",
        label: "Anthropic",
        needs_key: true,
        chat_url: None,
        messages_url: Some("https://api.anthropic.com/v1/messages"),
        format: Format::AnthropicMessages,
        messages_auth: Auth::XApiKey,
        key_page: "https://platform.claude.com/settings/keys",
        browser_login: None,
        default_model: "claude-haiku-4-5",
        note: Some(
            "needs an API key from the Claude Console; a Claude Pro/Max subscription is not one",
        ),
    },
    Preset {
        name: "openrouter",
        label: "OpenRouter",
        needs_key: true,
        chat_url: Some("https://openrouter.ai/api/v1/chat/completions"),
        messages_url: None,
        format: Format::OpenAiChat,
        messages_auth: Auth::Bearer,
        key_page: "https://openrouter.ai/settings/keys",
        browser_login: Some(crate::login::OPENROUTER),
        default_model: "anthropic/claude-haiku-4.5",
        note: None,
    },
    Preset {
        name: "opencode-go",
        label: "OpenCode Go",
        needs_key: true,
        chat_url: Some("https://opencode.ai/zen/go/v1/chat/completions"),
        messages_url: Some("https://opencode.ai/zen/go/v1/messages"),
        format: Format::OpenAiChat,
        messages_auth: Auth::Bearer,
        key_page: "https://opencode.ai/auth",
        browser_login: None,
        default_model: "glm-5.3-flash",
        note: Some(
            "OpenCode Go's terms say it is designed for coding agents; voice-assistant \
             traffic may not be what they allow. Your account, your call",
        ),
    },
    Preset {
        name: "ollama",
        label: "Ollama Cloud",
        needs_key: true,
        chat_url: Some("https://ollama.com/v1/chat/completions"),
        messages_url: None,
        format: Format::OpenAiChat,
        messages_auth: Auth::Bearer,
        key_page: "https://ollama.com/settings/keys",
        browser_login: None,
        default_model: "gpt-oss:120b",
        note: None,
    },
    Preset {
        name: "zai",
        label: "Z.ai",
        needs_key: true,
        chat_url: Some("https://api.z.ai/api/paas/v4/chat/completions"),
        messages_url: None,
        format: Format::OpenAiChat,
        messages_auth: Auth::Bearer,
        key_page: "https://z.ai/manage-apikey/apikey-list",
        browser_login: None,
        default_model: "glm-5.3-flash",
        note: None,
    },
    // A model on this machine, through any OpenAI-compatible local server.
    // Ollama's address is the default; LM Studio (:1234) and llama.cpp
    // (:8080) work by setting api_base. Granite 4.1 (Apache-2.0) is the
    // default model: it calls tools and answers without a thinking pass,
    // which a spoken reply can't afford.
    Preset {
        name: "local",
        label: "Local (on this computer)",
        needs_key: false,
        chat_url: Some("http://localhost:11434/v1/chat/completions"),
        messages_url: None,
        format: Format::OpenAiChat,
        messages_auth: Auth::Bearer,
        key_page: "https://ollama.com/download",
        browser_login: None,
        default_model: "granite4.1:8b",
        note: None,
    },
];

/// Where a chat-completions URL lists its models (`/v1/models`), for local
/// servers: Ollama, LM Studio and llama.cpp all serve it.
pub fn models_url(chat_url: &str) -> Option<String> {
    chat_url
        .strip_suffix("/chat/completions")
        .map(|base| format!("{base}/models"))
}

/// The model ids a local server offers. A short timeout: it's on this
/// machine, or it isn't running.
pub async fn local_models(chat_url: &str) -> Result<Vec<String>, String> {
    let url = models_url(chat_url).ok_or("api_base doesn't end in /chat/completions")?;
    let resp = reqwest::Client::new()
        .get(&url)
        .timeout(std::time::Duration::from_secs(2))
        .send()
        .await
        .map_err(|e| {
            if e.is_connect() {
                format!("no server at {url}")
            } else {
                e.to_string()
            }
        })?;
    if !resp.status().is_success() {
        return Err(format!("{url}: {}", resp.status()));
    }
    let body: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    let mut ids: Vec<String> = body["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| m["id"].as_str().map(str::to_owned))
        .collect();
    ids.sort();
    Ok(ids)
}

/// Whether a model a local server lists actually runs elsewhere: Ollama's
/// `:cloud` / `-cloud` models are forwarded to ollama.com, so what's said
/// to them leaves the machine.
pub fn runs_remotely(model: &str) -> bool {
    model.ends_with(":cloud") || model.ends_with("-cloud")
}

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
            // Only a local server is plain HTTP, and only on this machine.
            let scheme = if p.needs_key {
                "https://"
            } else {
                "http://localhost:"
            };
            assert!(e.url.starts_with(scheme), "{}", p.name);
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
    fn local_servers_list_their_models_beside_chat() {
        assert_eq!(
            models_url("http://localhost:11434/v1/chat/completions").as_deref(),
            Some("http://localhost:11434/v1/models")
        );
        assert_eq!(models_url("http://localhost:1234/v1/models"), None);
        assert!(runs_remotely("kimi-k3:cloud") && runs_remotely("gpt-oss:120b-cloud"));
        assert!(!runs_remotely("granite4.1:8b") && !runs_remotely("cloudy:7b"));
        let local = preset("local").unwrap();
        assert!(!local.needs_key && local.browser_login.is_none());
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
