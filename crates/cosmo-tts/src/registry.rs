//! Provider construction: the registry and what a provider is told at
//! construction (spec §2.2).
//!
//! Built-in providers register themselves as they land (OpenAI in §2.4,
//! Kokoro in §2.5); the daemon builds the registry it advertises from
//! config. An unknown name is a structured error that lists what *is*
//! available, so `cosmo doctor` can render the fix — the phase-1 discipline.

use std::collections::BTreeMap;

use cosmo_config::secret::SecretKey;

use crate::TtsError;
use crate::provider::VoiceProvider;

/// What a provider is told at construction. Grows only when a part needs a
/// field (per-provider extras such as model paths or test base-urls are
/// added by §2.4/§2.5 rather than guessed here).
#[derive(Debug, Clone, Default)]
pub struct ProviderInit {
    /// API key resolved through the §1.4 source (env var → Secret Service).
    /// Cloud providers take it from here — never from `config.ron`.
    pub api_key: Option<SecretKey>,
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
                available: self.names().join(", "),
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
            .create("fake", &ProviderInit { api_key: Some(key) })
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

    #[test]
    fn provider_init_debug_does_not_leak_the_key() {
        // ProviderInit derives Debug and carries a SecretKey; the redacting
        // newtype must keep that derivation safe.
        let init = ProviderInit {
            api_key: Some(SecretKey::from_raw("sk-test-SECRET".into())),
        };
        assert!(!format!("{init:?}").contains("sk-test-SECRET"));
        assert!(format!("{init:?}").contains("[redacted]"));
    }
}
