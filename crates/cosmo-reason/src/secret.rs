//! Secret handling (plan §1.4 — decided, not a menu).
//!
//! Resolution order, exactly: `OPENAI_API_KEY` env var → Secret Service
//! (oo7) → structured error pointing at `cosmo auth login`. The env var is
//! for dev and CI only. The key is wrapped in [`SecretKey`], whose
//! `Debug`/`Display` print `[redacted]`, so it cannot reach a `tracing`
//! span, an error chain, or a transcript log by accident.
//!
//! The [`SecretKey`] newtype itself lives in `cosmo-config::secret` (next to
//! the "never a credential" policy it enforces) and is re-exported here so
//! `cosmo_reason::secret::SecretKey` keeps resolving; cloud consumers outside
//! this crate (OpenAI TTS, phase 2) take it from `cosmo-config` too, without
//! depending on the reasoner.

use std::fmt;

use oo7::Keyring;

pub use cosmo_config::secret::SecretKey;

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
            return Ok(SecretKey::from_raw(key));
        }
        // Secret Service via oo7 on the runtime (oo7 is async-only). The
        // daemon calls this from its first reasoning turn; a locked or
        // empty keyring is a *retry on next use*, not a startup failure
        // (plan §1.4).
        // `keyring_lookup` yields a [`ReasonKind`], whose Display is the
        // user-facing fix. Mapping an `oo7::Error` here instead renders
        // oo7's own Display and produces nonsense like *"DBus error The
        // collection 'no key stored — run `cosmo auth login`' doesn't
        // exists"* — the message smuggled through a D-Bus error's
        // collection-name field and back out again.
        match tokio::task::block_in_place(|| {
            tokio::runtime::Handle::try_current().map(|h| h.block_on(keyring_lookup()))
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
    keyring_lookup().await
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
    /// Classify a real error from the Secret Service.
    ///
    /// Only genuine oo7 errors reach this. Absence and lockedness are
    /// detected directly in [`keyring_lookup`] and returned as their own
    /// variants — fabricating an `oo7::Error` to carry a message means the
    /// message comes back out through oo7's `Display`, wrapped in whatever
    /// that variant's sentence happens to be.
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
///
/// Returns [`ReasonKind`] rather than `oo7::Error` so that the three states
/// plan §1.4 requires — locked / missing / broken — are *the* return type,
/// and every caller renders the same actionable sentence.
async fn keyring_lookup() -> Result<SecretKey, ReasonKind> {
    let keyring = Keyring::new().await.map_err(|e| ReasonKind::from_oo7(&e))?;
    // Locked ≠ crash: surface it, let the daemon retry next turn.
    if keyring
        .is_locked()
        .await
        .map_err(|e| ReasonKind::from_oo7(&e))?
    {
        return Err(ReasonKind::KeyringLocked);
    }
    let items = keyring
        .search_items(&[("application", "cosmo"), ("provider", "openai")])
        .await
        .map_err(|e| ReasonKind::from_oo7(&e))?;
    let Some(item) = items.into_iter().next() else {
        return Err(ReasonKind::Missing);
    };
    let secret = item.secret().await.map_err(|e| ReasonKind::from_oo7(&e))?;
    let text = String::from_utf8_lossy(secret.as_bytes()).to_string();
    Ok(SecretKey::from_raw(text))
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

    // Redaction tests for the newtype itself live with the type, in
    // cosmo-config::secret.

    /// Plan §1.4 requires three *actionable* states. The daemon's `say`
    /// path renders `ReasonKind` directly, so its Display is the message a
    /// user sees on a keyless machine — it must name the fix, not leak a
    /// transport error. This previously reached the CLI as
    /// *"DBus error The collection 'no key stored — run `cosmo auth login`'
    /// doesn't exists"*: the message had been stuffed into an `oo7` error's
    /// collection-name field and rendered back out through oo7's Display.
    #[test]
    fn key_failures_render_their_fix() {
        let missing = ReasonKind::Missing.to_string();
        assert_eq!(missing, "no key stored — run `cosmo auth login`");
        assert!(
            !missing.contains("DBus"),
            "transport detail leaked: {missing}"
        );
        assert!(!missing.contains("collection"));

        let locked = ReasonKind::KeyringLocked.to_string();
        assert_eq!(locked, "keyring locked — unlock and retry");
        assert!(!locked.contains("DBus"));

        // A genuine transport failure keeps its detail *and* names the fix.
        let failed = ReasonKind::Failed("connection refused".into()).to_string();
        assert!(failed.contains("connection refused"));
        assert!(failed.contains("cosmo auth login"));
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
                Ok(SecretKey::from_raw(k))
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
