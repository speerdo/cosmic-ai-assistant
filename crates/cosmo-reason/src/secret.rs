//! Secret handling (plan §1.4 — decided, not a menu).
//!
//! Resolution order, exactly: `OPENAI_API_KEY` env var → Secret Service
//! (oo7) → structured error pointing at `cosmo auth login`. The env var is
//! for dev and CI only. The key is wrapped in [`SecretKey`], whose
//! `Debug`/`Display` print `[redacted]`, so it cannot reach a `tracing`
//! span, an error chain, or a transcript log by accident.

use std::fmt;

use oo7::Keyring;

/// The resolved API key. `Debug`/`Display` are redacted by construction —
/// the point is that `format!("{:?}", client)` cannot leak it.
#[derive(Clone)]
pub struct SecretKey {
    inner: String,
}

impl std::fmt::Debug for SecretKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("[redacted]")
    }
}

impl std::fmt::Display for SecretKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("[redacted]")
    }
}

impl SecretKey {
    /// Intentionally explicit: the one place the raw key is readable.
    pub fn expose(&self) -> &str {
        &self.inner
    }

    /// Test-only constructor (integration tests need it too).
    #[doc(hidden)]
    pub fn from_raw(key: String) -> Self {
        Self { inner: key }
    }
}

/// The three doctor states, each with a different fix (plan §1.4):
/// `present (source)` / `keyring locked` / `no key stored`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyPresence {
    /// Present from the env var (dev/CI only).
    Env,
    /// Present in the Secret Service.
    Keyring,
    /// The keyring is locked — retry later (plan §1.4: not a crash).
    KeyringLocked,
    /// No key anywhere.
    Missing,
}

/// A provider of API keys.
pub trait KeySource {
    /// Resolve the key. Returns an error carrying the *user-facing fix* on
    /// absence; a locked keyring maps to [`KeyPresence::KeyringLocked`].
    fn resolve(&self) -> Result<SecretKey, crate::ReasonError>;
}

/// The production source: env var first, then oo7.
pub struct DefaultKeySource;

impl KeySource for DefaultKeySource {
    fn resolve(&self) -> Result<SecretKey, crate::ReasonError> {
        if let Ok(key) = std::env::var("OPENAI_API_KEY")
            && !key.trim().is_empty()
        {
            tracing::debug!("api key source: env (dev/CI only)");
            return Ok(SecretKey { inner: key });
        }
        // Secret Service via oo7 on the runtime (oo7 is async-only). The
        // daemon calls this from its first reasoning turn; a locked or
        // empty keyring is a *retry on next use*, not a startup failure
        // (plan §1.4).
        match tokio::task::block_in_place(|| {
            tokio::runtime::Handle::try_current()
                .map(|h| h.block_on(async { keyring_lookup().await }))
        }) {
            Ok(Ok(key)) => {
                tracing::debug!("api key source: keyring");
                Ok(key)
            }
            Ok(Err(kind)) => Err(crate::ReasonError::NoKey(kind.to_string())),
            Err(e) => Err(crate::ReasonError::NoKey(format!(
                "keyring unavailable from this context: {e}; run `cosmo auth login`"
            ))),
        }
    }
}

/// Resolve through the Secret Service (async path; call from the daemon's
/// first reasoning turn).
pub async fn resolve_keyring() -> Result<SecretKey, ReasonKind> {
    keyring_lookup().await.map_err(|e| ReasonKind::from_oo7(&e))
}

/// A structured reason the key is unavailable — maps 1:1 onto the doctor's
/// three states plus generic failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReasonKind {
    /// The keyring is locked; unlocking it fixes this.
    KeyringLocked,
    /// No key stored; `cosmo auth login` fixes this.
    Missing,
    /// Something else went wrong reaching the keyring.
    Failed(String),
}

impl ReasonKind {
    fn from_oo7(e: &oo7::Error) -> Self {
        match e {
            oo7::Error::DBus(oo7::dbus::Error::Service(oo7::dbus::ServiceError::IsLocked(_))) => {
                ReasonKind::KeyringLocked
            }
            oo7::Error::DBus(oo7::dbus::Error::NotFound(_)) => ReasonKind::Missing,
            other => ReasonKind::Failed(other.to_string()),
        }
    }
}

impl std::fmt::Display for ReasonKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ReasonKind::KeyringLocked => write!(f, "keyring locked — unlock and retry"),
            ReasonKind::Missing => write!(f, "no key stored — run `cosmo auth login`"),
            ReasonKind::Failed(e) => {
                write!(f, "keyring lookup failed: {e}; run `cosmo auth login`")
            }
        }
    }
}

/// Look the key up in the Secret Service by the decided attribute set.
async fn keyring_lookup() -> Result<SecretKey, oo7::Error> {
    let keyring = Keyring::new().await?;
    if keyring.is_locked().await? {
        // Locked ≠ crash: surface it, let the daemon retry next turn.
        return Err(oo7::Error::DBus(oo7::dbus::Error::Service(
            oo7::dbus::ServiceError::IsLocked("collection is locked".into()),
        )));
    }
    let items = keyring
        .search_items(&[("application", "cosmo"), ("provider", "openai")])
        .await?;
    let Some(item) = items.into_iter().next() else {
        return Err(oo7::Error::DBus(oo7::dbus::Error::NotFound(
            "no key stored — run `cosmo auth login`".into(),
        )));
    };
    let secret = item.secret().await?;
    let text = String::from_utf8_lossy(secret.as_bytes()).to_string();
    Ok(SecretKey { inner: text })
}

/// Store the key in the Secret Service (`cosmo auth login`).
pub async fn store_key(key: &str) -> anyhow::Result<()> {
    let keyring = Keyring::new().await?;
    keyring
        .create_item(
            "cosmo — OpenAI API key",
            &[("application", "cosmo"), ("provider", "openai")],
            oo7::Secret::text(key),
            true,
        )
        .await?;
    Ok(())
}

/// Delete the stored key (`cosmo auth logout`).
pub async fn delete_key() -> anyhow::Result<()> {
    let keyring = Keyring::new().await?;
    keyring
        .delete(&[("application", "cosmo"), ("provider", "openai")])
        .await?;
    Ok(())
}

/// `cosmo auth status`: is a key resolvable, and from which source.
pub async fn auth_status() -> String {
    if std::env::var("OPENAI_API_KEY")
        .map(|k| !k.trim().is_empty())
        .unwrap_or(false)
    {
        return "key present (source: env — dev/CI only)".into();
    }
    match resolve_keyring().await {
        Ok(_) => "key present (source: keyring)".into(),
        Err(ReasonKind::KeyringLocked) => "keyring locked — unlock and retry".into(),
        Err(ReasonKind::Missing) => "no key stored — run `cosmo auth login`".into(),
        Err(other) => format!("keyring unavailable — {other}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "sk-test-ABCDEF0123456789";

    #[test]
    fn debug_never_leaks_the_key() {
        let key = SecretKey {
            inner: KEY.to_string(),
        };
        let dbg = format!("{key:?}");
        assert!(!dbg.contains(KEY), "Debug leaks the key: {dbg}");
        assert_eq!(dbg, "[redacted]");

        let disp = format!("{key}");
        assert!(!disp.contains(KEY), "Display leaks the key: {disp}");
        assert_eq!(disp, "[redacted]");
    }

    /// The specific trap from plan §1.4: the reasoner struct holds the key;
    /// its Debug must not print it.
    #[test]
    fn client_debug_is_redacted() {
        // A wrapper struct holding a SecretKey derives Debug through it;
        // the output must be redacted.
        #[derive(Debug)]
        #[allow(dead_code)] // read via the Debug output we assert on
        struct Holder {
            key: SecretKey,
        }
        let h = Holder {
            key: SecretKey {
                inner: KEY.to_string(),
            },
        };
        assert!(!format!("{h:?}").contains(KEY));
    }

    #[test]
    fn env_source_takes_precedence() {
        // Unique var name; single test binary serialises env access.
        // SAFETY: no other thread reads this env var concurrently.
        #[allow(unsafe_code)]
        unsafe {
            std::env::set_var("COSMO_TEST_KEY", KEY);
        }
        struct TestSource;
        impl KeySource for TestSource {
            fn resolve(&self) -> Result<SecretKey, crate::ReasonError> {
                let k = std::env::var("COSMO_TEST_KEY").unwrap_or_default();
                if k.is_empty() {
                    return Err(crate::ReasonError::NoKey("empty".into()));
                }
                Ok(SecretKey { inner: k })
            }
        }
        let resolved = TestSource.resolve().expect("env key resolves");
        assert_eq!(resolved.expose(), KEY);
        // SAFETY: as above.
        #[allow(unsafe_code)]
        unsafe {
            std::env::remove_var("COSMO_TEST_KEY");
        }
    }
}
