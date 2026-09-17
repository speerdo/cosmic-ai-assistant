//! The redacting API-key newtype (plan §1.4).
//!
//! This lives in `cosmo-config` — next to the "never a credential" rule it
//! enforces — rather than in `cosmo-reason`, because every cloud consumer
//! needs it (reasoning since phase 1, OpenAI TTS from phase 2 §2.4) and the
//! dependency arrows must stay usable in both directions (`cosmo-reason`
//! will call `cosmo-tts` for sentence streaming in phase 5).
//!
//! `Debug`/`Display` print `[redacted]`, so the key cannot reach a `tracing`
//! span, an error chain, or a transcript log by accident. Where the key is
//! *stored and resolved* is `cosmo-reason::secret` (env var → Secret
//! Service → structured error); this type is only the container.

use std::fmt;

/// The resolved API key. `Debug`/`Display` are redacted by construction —
/// the point is that `format!("{:?}", client)` cannot leak it.
#[derive(Clone)]
pub struct SecretKey {
    inner: String,
}

impl fmt::Debug for SecretKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[redacted]")
    }
}

impl fmt::Display for SecretKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[redacted]")
    }
}

impl SecretKey {
    /// Intentionally explicit: the one place the raw key is readable.
    pub fn expose(&self) -> &str {
        &self.inner
    }

    /// Wrap a key already resolved by a key source — the constructor
    /// `cosmo_reason::secret::KeySource` implementations build through
    /// (that trait lives with the env-var/Secret-Service resolution, not
    /// with the container). The raw string does not escape the type once
    /// wrapped.
    pub fn from_raw(key: String) -> Self {
        Self { inner: key }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "sk-test-ABCDEF0123456789";

    #[test]
    fn debug_never_leaks_the_key() {
        let key = SecretKey::from_raw(KEY.to_string());
        let dbg = format!("{key:?}");
        assert!(!dbg.contains(KEY), "Debug leaks the key: {dbg}");
        assert_eq!(dbg, "[redacted]");

        let disp = format!("{key}");
        assert!(!disp.contains(KEY), "Display leaks the key: {disp}");
        assert_eq!(disp, "[redacted]");
    }

    /// The specific trap from plan §1.4: a wrapper struct holding the key;
    /// its derived Debug must not print it.
    #[test]
    fn holder_debug_is_redacted() {
        #[derive(Debug)]
        #[allow(dead_code)] // read via the Debug output we assert on
        struct Holder {
            key: SecretKey,
        }
        let h = Holder {
            key: SecretKey::from_raw(KEY.to_string()),
        };
        assert!(!format!("{h:?}").contains(KEY));
    }

    #[test]
    fn expose_reads_and_clone_stays_redacted() {
        let key = SecretKey::from_raw(KEY.to_string());
        assert_eq!(key.expose(), KEY);
        let copy = key.clone();
        assert_eq!(copy.expose(), KEY);
        assert_eq!(format!("{copy:?}"), "[redacted]");
    }
}
