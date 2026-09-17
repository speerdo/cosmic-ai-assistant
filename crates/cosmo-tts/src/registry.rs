//! Provider construction: the registry and what a provider is told at
//! construction (spec §2.2).
//!
//! Built-in providers register themselves as they land (OpenAI in §2.4,
//! Kokoro in §2.5); the daemon builds the registry it advertises from
//! config. An unknown name is a structured error that lists what *is*
//! available, so `cosmo doctor` can render the fix — the phase-1 discipline.

use std::collections::BTreeMap;
use std::time::Duration;

use cosmo_config::secret::SecretKey;

use crate::TtsError;
use crate::provider::VoiceProvider;

/// What a provider is told at construction. Grows only when a part needs a
/// field (Kokoro's model-pack paths, §2.5, are the next expected addition).
#[derive(Debug, Clone, Default)]
pub struct ProviderInit {
    /// API key resolved through the §1.4 source (env var → Secret Service).
    /// Cloud providers take it from here — never from `config.ron`.
    pub api_key: Option<SecretKey>,
    /// Endpoint override for providers that talk HTTP (tests, self-hosted
    /// gateways). `None` → the provider's production default.
    pub base_url: Option<String>,
    /// Model for providers that expose one (OpenAI: `gpt-4o-mini-tts`;
    /// §2.5's Kokoro reuses this for its model pack). `None` → provider
    /// default.
    pub model: Option<String>,
    /// Optional speaking-style instruction (OpenAI's `instructions`:
    /// affect, tone, pacing; sourced from `voice_instructions` in config).
    /// `None`/empty → the field is omitted from the request.
    pub instructions: Option<String>,
    /// Ceiling on one network synthesis, for providers that talk HTTP.
    /// `None` → the provider's default. A hung request is not one of the
    /// §1.4 states: the caller is waiting to *hear* something, so it must
    /// fail with a reason rather than never return.
    pub request_timeout: Option<Duration>,
}

/// Constructs a provider. A plain fn pointer keeps the registry cheap and
/// cloneable; providers needing runtime state close over it internally.
pub type ProviderFactory = fn(&ProviderInit) -> Result<Box<dyn VoiceProvider>, TtsError>;

/// Name → constructor. Order-independent (`BTreeMap`), so the
/// unknown-provider error and `names()` list alphabetically.
#[derive(Debug, Clone, Default)]
pub struct Registry {
    factories: BTreeMap<String, ProviderFactory>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Every provider built into this build. Grows as the spec's parts land
    /// (`openai` in §2.4, `kokoro` in §2.5, `piper` in §2.8) — the daemon
    /// constructs its registry from this, so a provider that exists in the
    /// tree is one the config can name.
    pub fn with_builtins() -> Self {
        let mut reg = Self::new();
        reg.register("openai", crate::openai::factory);
        reg
    }

    pub fn register(&mut self, name: impl Into<String>, factory: ProviderFactory) {
        self.factories.insert(name.into(), factory);
    }

    /// Every registered provider name.
    pub fn names(&self) -> Vec<&str> {
        self.factories.keys().map(String::as_str).collect()
    }

    /// Construct the named provider, or explain what is available.
    pub fn create(
        &self,
        name: &str,
        init: &ProviderInit,
    ) -> Result<Box<dyn VoiceProvider>, TtsError> {
        let factory = self
            .factories
            .get(name)
            .ok_or_else(|| TtsError::UnknownProvider {
                name: name.to_owned(),
                // An empty registry would otherwise render "(available: )",
                // which reads like a truncated message rather than a state.
                available: if self.factories.is_empty() {
                    "none registered".to_owned()
                } else {
                    self.names().join(", ")
                },
            })?;
        factory(init)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pcm::Pcm;
    use crate::provider::{Accent, Gender, LatencyClass, Voice, VoiceProvider};
    use futures::future::{BoxFuture, ready};

    /// Stands in for a real provider in registry tests.
    struct FakeProvider;

    impl VoiceProvider for FakeProvider {
        fn id(&self) -> &str {
            "fake"
        }

        fn list_voices(&self) -> Vec<Voice> {
            vec![Voice {
                id: "fv1".into(),
                label: "Fake voice".into(),
                accent: Accent::from_code("en_GB"),
                gender: Some(Gender::Female),
                sample: None,
            }]
        }

        fn synthesize(&self, text: &str, _voice: &str) -> BoxFuture<'_, Result<Pcm, TtsError>> {
            let n = text.len();
            Box::pin(ready(Ok(Pcm::new(24_000, vec![0.0; n]))))
        }

        fn is_local(&self) -> bool {
            true
        }

        fn latency_class(&self) -> LatencyClass {
            LatencyClass::Fast
        }
    }

    fn fake_factory(init: &ProviderInit) -> Result<Box<dyn VoiceProvider>, TtsError> {
        // The init reaches the provider; the key type stays opaque here (its
        // redaction is tested in cosmo-config::secret).
        if let Some(key) = &init.api_key {
            assert_eq!(key.expose(), "sk-test");
        }
        Ok(Box::new(FakeProvider))
    }

    #[test]
    fn create_passes_init_and_lists_names() {
        let mut reg = Registry::new();
        reg.register("fake", fake_factory);
        assert_eq!(reg.names(), vec!["fake"]);

        let key = SecretKey::from_raw("sk-test".into());
        let provider = reg
            .create(
                "fake",
                &ProviderInit {
                    api_key: Some(key),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(provider.id(), "fake");
        assert!(provider.is_local());
        assert_eq!(provider.latency_class(), LatencyClass::Fast);
        assert_eq!(provider.list_voices().len(), 1);
    }

    #[test]
    fn unknown_provider_names_the_available_ones() {
        let mut reg = Registry::new();
        reg.register("fake", fake_factory);
        reg.register("other", fake_factory);
        let err = match reg.create("kokoro", &ProviderInit::default()) {
            Err(e) => e,
            Ok(_) => panic!("an unregistered name must not construct"),
        };
        assert!(matches!(err, TtsError::UnknownProvider { .. }));
        let msg = err.to_string();
        assert!(msg.contains("kokoro"), "{msg}");
        assert!(msg.contains("fake") && msg.contains("other"), "{msg}");
    }

    /// `with_builtins` is what the daemon constructs its registry from, so a
    /// builtin that exists in the tree but never registers would surface as
    /// "unknown provider" for a name the config legitimately allows.
    #[test]
    fn builtins_are_registered_under_their_config_names() {
        assert!(
            Registry::with_builtins().names().contains(&"openai"),
            "openai must be constructible by the name config uses"
        );
    }

    #[test]
    fn empty_registry_says_so_instead_of_trailing_off() {
        let msg = match Registry::new().create("openai", &ProviderInit::default()) {
            Err(e) => e.to_string(),
            Ok(_) => panic!("an empty registry must not construct anything"),
        };
        assert!(msg.contains("none registered"), "{msg}");
    }

    #[test]
    fn provider_init_debug_does_not_leak_the_key() {
        // ProviderInit derives Debug and carries a SecretKey; the redacting
        // newtype must keep that derivation safe.
        let init = ProviderInit {
            api_key: Some(SecretKey::from_raw("sk-test-SECRET".into())),
            ..Default::default()
        };
        assert!(!format!("{init:?}").contains("sk-test-SECRET"));
        assert!(format!("{init:?}").contains("[redacted]"));
    }
}
