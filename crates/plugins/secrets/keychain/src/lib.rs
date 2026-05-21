//! `devboy-secret-keychain` — built-in `SecretSource` for the OS
//! keychain per [ADR-021] §8.
//!
//! A refactor of the legacy
//! [`devboy_storage::KeychainStore`]
//! behind the [`SecretSource`] trait. The keychain is the only
//! source where the ADR-020 path is also the backend reference —
//! no transformation, no joining.
//!
//! # Capabilities
//!
//! `READ | LIST | VALIDATE | WRITE`. The keychain is **credential-
//! free** ([`SecretSource::requires_credential`] returns `None`),
//! which is what makes it eligible (alongside `local-vault`) to
//! hold source-credentials per ADR-021 §4.
//!
//! # Availability
//!
//! - macOS: always available — Keychain Services is part of the OS.
//! - Windows: always available — Credential Manager is part of the
//!   OS.
//! - Linux: depends on the user's session having a running
//!   Secret Service (GNOME Keyring or KWallet via D-Bus). Headless
//!   Linux containers without a session typically don't —
//!   `is_available()` returns [`SourceStatus::NotInstalled`] and
//!   the router falls back to `local-vault` per ADR-021 §2 step 3.
//! - Locked sessions (macOS Keychain explicitly locked, GNOME
//!   Keyring locked) surface as [`SourceStatus::Locked`].
//!
//! # Enumeration
//!
//! The `keyring` crate does not currently expose
//! `SecKeychainSearchCreateFromAttributes` (macOS), `SearchItems`
//! (Linux Secret Service), or `CredEnumerate` (Windows). Until it
//! does, [`KeychainSource::list`] returns an empty vector — the
//! `LIST` capability is declared honestly (the source *can*
//! support enumeration in principle) but the implementation is
//! deferred to a follow-up that wires the platform-specific
//! search APIs directly. Discovery flows that need an inventory
//! today should fall back to the user's manifest.
//!
//! [ADR-021]: https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-021-external-secret-sources.md

use async_trait::async_trait;
use devboy_storage::{
    Capabilities, GetOutcome, RemoteRef, SecretSource, SourceError, SourceStatus,
};
use secrecy::SecretString;
use tracing::debug;

/// Default keychain service name — matches the value the legacy
/// [`devboy_storage::KeychainStore`] uses, so a `KeychainSource`
/// constructed with [`KeychainSource::new`] sees the same entries
/// as code still on the legacy API during the migration.
pub const DEFAULT_SERVICE_NAME: &str = "devboy-tools";

/// `SecretSource` backed by the OS keychain.
///
/// The `name` field is the user-facing source identifier from
/// `[[source]] name = "..."` in `sources.toml`; the `service_name`
/// is what the OS keychain layers store entries under (typically
/// the same `"devboy-tools"` constant).
#[derive(Debug, Clone)]
pub struct KeychainSource {
    /// Logical name from `sources.toml`.
    name: String,
    /// OS keychain service name. Tests pass a unique string to
    /// avoid clobbering real entries.
    service_name: String,
}

impl KeychainSource {
    /// Build a source named `name`, using the workspace's default
    /// keychain service name. Equivalent to `[[source]] name = "..."
    /// type = "keychain"` with no overrides.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            service_name: DEFAULT_SERVICE_NAME.to_owned(),
        }
    }

    /// Build a source with a custom keychain service name. Used by
    /// integration tests so they don't write into the user's real
    /// `devboy-tools` entries.
    pub fn with_service_name(name: impl Into<String>, service_name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            service_name: service_name.into(),
        }
    }

    /// OS keychain service name this source operates against.
    pub fn service_name(&self) -> &str {
        &self.service_name
    }

    fn entry(&self, key: &str) -> Result<keyring::Entry, keyring::Error> {
        keyring::Entry::new(&self.service_name, key)
    }
}

#[async_trait]
impl SecretSource for KeychainSource {
    fn name(&self) -> &str {
        &self.name
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::READ | Capabilities::LIST | Capabilities::VALIDATE | Capabilities::WRITE
    }

    async fn is_available(&self) -> SourceStatus {
        // Probe by attempting to construct an entry and reading a
        // path that almost certainly does not exist. The error
        // shape tells us *why* the backend can't serve us.
        let probe = match self.entry("__devboy_availability_check__") {
            Ok(e) => e,
            Err(_) => return SourceStatus::NotInstalled,
        };
        match probe.get_password() {
            Ok(_) => SourceStatus::Available,
            Err(keyring::Error::NoEntry) => SourceStatus::Available,
            Err(keyring::Error::NoStorageAccess(e)) => {
                debug!(error = %e, "keychain is locked / access denied");
                SourceStatus::Locked
            }
            Err(keyring::Error::PlatformFailure(e)) => {
                debug!(error = %e, "keychain platform failure — treating as NotInstalled");
                SourceStatus::NotInstalled
            }
            Err(e) => SourceStatus::Error(e.to_string()),
        }
    }

    async fn get(&self, reference: &str) -> Result<Option<GetOutcome>, SourceError> {
        if reference.is_empty() {
            return Err(SourceError::BadReference {
                name: self.name.clone(),
                reference: reference.to_owned(),
                reason: "reference must be a non-empty path".to_owned(),
            });
        }
        let entry = self.entry(reference).map_err(|e| SourceError::Upstream {
            name: self.name.clone(),
            message: format!("failed to construct keychain entry: {e}"),
        })?;
        match entry.get_password() {
            Ok(s) => Ok(Some(GetOutcome {
                value: SecretString::from(s),
                lease_duration: None,
            })),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(keyring::Error::NoStorageAccess(_)) => Err(SourceError::Locked {
                name: self.name.clone(),
            }),
            Err(e) => Err(SourceError::Upstream {
                name: self.name.clone(),
                message: e.to_string(),
            }),
        }
    }

    async fn list(&self) -> Result<Vec<RemoteRef>, SourceError> {
        // The `keyring` crate does not currently expose enumeration
        // primitives. Return an empty list rather than `Err` so the
        // router's "declared cap → call works" invariant holds; the
        // module-level docs explain the limitation.
        Ok(Vec::new())
    }

    async fn validate(&self, reference: &str) -> Result<(), SourceError> {
        // For the keychain, "valid reference" means the path string
        // is non-empty. Existence-of-entry checks belong in `get`.
        if reference.is_empty() {
            return Err(SourceError::BadReference {
                name: self.name.clone(),
                reference: reference.to_owned(),
                reason: "reference must be a non-empty path".to_owned(),
            });
        }
        Ok(())
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // -- Pure-data trait surface ------------------------------------

    #[test]
    fn name_returns_constructor_value() {
        let s = KeychainSource::new("keychain");
        assert_eq!(s.name(), "keychain");
    }

    #[test]
    fn service_name_defaults_to_devboy_tools() {
        let s = KeychainSource::new("keychain");
        assert_eq!(s.service_name(), DEFAULT_SERVICE_NAME);
    }

    #[test]
    fn with_service_name_overrides_default() {
        let s = KeychainSource::with_service_name("keychain", "custom-test");
        assert_eq!(s.service_name(), "custom-test");
    }

    #[test]
    fn capabilities_match_adr_021_section_8_keychain() {
        let s = KeychainSource::new("keychain");
        let caps = s.capabilities();
        assert!(caps.contains(Capabilities::READ));
        assert!(caps.contains(Capabilities::LIST));
        assert!(caps.contains(Capabilities::VALIDATE));
        assert!(caps.contains(Capabilities::WRITE));
        // Keychain doesn't natively prompt for biometrics in this
        // wrapper (Touch-ID-wrapped entries are local-vault's
        // territory, ADR-023), is read silently, and doesn't
        // declare ROTATE.
        assert!(!caps.contains(Capabilities::ROTATE));
        assert!(!caps.contains(Capabilities::BIOMETRIC_PROMPT));
        assert!(!caps.contains(Capabilities::AUDIT_LOGGED));
    }

    #[test]
    fn requires_credential_returns_none_keychain_is_credential_free() {
        // ADR-021 §4: keychain (and local-vault) are credential-
        // free, which is what makes them eligible to hold source-
        // credentials.
        let s = KeychainSource::new("keychain");
        assert!(s.requires_credential().is_none());
    }

    // -- Validate ---------------------------------------------------

    #[tokio::test]
    async fn validate_accepts_non_empty_reference() {
        let s = KeychainSource::new("keychain");
        s.validate("team/gitlab/token-deploy").await.unwrap();
    }

    #[tokio::test]
    async fn validate_rejects_empty_reference() {
        let s = KeychainSource::new("keychain");
        let err = s.validate("").await.unwrap_err();
        match err {
            SourceError::BadReference {
                name,
                reference,
                reason,
            } => {
                assert_eq!(name, "keychain");
                assert_eq!(reference, "");
                assert!(reason.contains("non-empty"));
            }
            other => panic!("expected BadReference, got {other:?}"),
        }
    }

    // -- Get error path --------------------------------------------

    #[tokio::test]
    async fn get_rejects_empty_reference() {
        let s = KeychainSource::new("keychain");
        let err = s.get("").await.unwrap_err();
        assert!(matches!(err, SourceError::BadReference { .. }));
    }

    // -- List ------------------------------------------------------

    #[tokio::test]
    async fn list_returns_empty_vec_until_native_enumeration_lands() {
        // Documented limitation — see the module docs.
        let s = KeychainSource::new("keychain");
        let v = s.list().await.unwrap();
        assert!(v.is_empty());
    }

    // -- is_available smoke ----------------------------------------

    #[tokio::test]
    async fn is_available_returns_a_known_variant() {
        // We can't predict which variant the test environment
        // will hit (CI may not have a keychain; macOS dev machine
        // typically has Available), but the call must not panic
        // and must return one of the documented variants.
        let s = KeychainSource::new("keychain-availability-probe");
        let status = s.is_available().await;
        let recognised = matches!(
            status,
            SourceStatus::Available
                | SourceStatus::Locked
                | SourceStatus::NotInstalled
                | SourceStatus::Error(_)
        );
        assert!(recognised, "is_available returned an unrecognised variant");
    }

    // -- dyn trait usage -------------------------------------------

    #[tokio::test]
    async fn keychain_source_works_through_dyn_box() {
        // The router (P5+) will store sources in
        // `HashMap<String, Box<dyn SecretSource>>`. Confirm dyn
        // compatibility for this concrete impl.
        let s: Box<dyn SecretSource> = Box::new(KeychainSource::new("keychain"));
        assert_eq!(s.name(), "keychain");
        assert!(s.requires_credential().is_none());
        assert!(s.capabilities().contains(Capabilities::READ));
    }

    // -- macOS integration -----------------------------------------

    /// Real-keychain round-trip on macOS. Cfg-gated so the test
    /// compiles only on macOS; uses a uniquely-named service so it
    /// cannot clobber the user's actual `devboy-tools` entries.
    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn macos_round_trip_via_real_keychain() {
        use devboy_storage::CredentialStore;
        use secrecy::ExposeSecret;

        let service = format!(
            "devboy-tools-secret-source-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        );
        let s = KeychainSource::with_service_name("keychain-test", &service);
        let key = "team/test/round-trip";
        let value = SecretString::from("integration-test-value".to_owned());

        // 1) Initial state: no entry. The trait reports `Ok(None)`.
        let initial = s.get(key).await.unwrap();
        assert!(initial.is_none());

        // 2) Write through the legacy KeychainStore — the trait
        //    does not yet expose `put` (P11+ wires writes through
        //    the router), but the source's WRITE capability is
        //    honest because the underlying backend supports it.
        let underlying = devboy_storage::KeychainStore::with_service_name(&service);
        underlying.store(key, &value).unwrap();

        // 3) Read through the trait. lease_duration is None (the
        //    keychain has no native lease concept).
        let read = s
            .get(key)
            .await
            .unwrap()
            .expect("expected Some after write");
        assert_eq!(read.value.expose_secret(), "integration-test-value");
        assert!(read.lease_duration.is_none());

        // 4) Cleanup.
        underlying.delete(key).unwrap();

        // 5) Confirm the entry is gone.
        let after_delete = s.get(key).await.unwrap();
        assert!(after_delete.is_none());
    }
}
