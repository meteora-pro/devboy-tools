//! Read-only view of the OS keychain, for secrets written before
//! it left the default credential chain.
//!
//! # Why this exists
//!
//! [ADR-024] §6 took the OS keychain out of the default chain.
//! For a new install that is simply the design. For an install
//! that predates it, every secret the user owns lives in the
//! keychain, and the upgrade turns "resolve the GitLab token"
//! into "no such secret" — with remediation text suggesting an
//! environment variable, as though the token had never existed.
//! The secret is still on disk, still decryptable, and the tool
//! has stopped looking at it.
//!
//! # The asymmetry this is built on
//!
//! Reading old secrets out of the keychain is safe. Writing new
//! ones into it is the thing §6 set out to stop, because that is
//! what creates the dependency in the first place. So this store
//! reads and refuses to write: existing users keep working
//! without new users acquiring the dependency.
//!
//! # Why it is loud
//!
//! A silent fallback is permanent. Nobody migrates off a thing
//! that costs them nothing, and two releases later it cannot be
//! removed because everyone still depends on it. Every secret
//! served this way logs a warning naming the release the fallback
//! disappears in, and `devboy doctor` reports the entries that
//! are being kept alive by it.
//!
//! The warning fires once per key per process rather than once
//! per read: a single command can resolve the same secret several
//! times, and a wall of identical lines is read as noise and
//! filtered out, which is the same as being silent.
//!
//! # How it turns itself off
//!
//! `devboy secrets migrate` moves the values into the chain
//! proper and sets `secrets.migration_complete`. The chain then
//! stops appending this store — see the caller — and the warnings
//! stop with it.
//!
//! [ADR-024]: https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-024-agent-mediated-secret-access.md

use std::collections::HashSet;
use std::sync::Mutex;

use devboy_core::{Error, Result};
use secrecy::SecretString;
use tracing::warn;

use crate::{CredentialStore, KeychainStore};

/// The release this fallback is removed in.
///
/// Named in every warning and in the write error, because "this
/// is deprecated" without a date is something a reader can defer
/// forever. Bounded by version rather than by calendar date: a
/// user upgrading from 0.33 to 0.36 in one jump gets the same
/// warning the whole way, and it stops being true at exactly the
/// point the code changes.
pub const FALLBACK_REMOVED_IN: &str = "0.36";

/// Reads the OS keychain; refuses to write to it.
///
/// Belongs **last** in the credential chain: it is the answer of
/// last resort, after the environment, the local vault, and an
/// explicitly opted-in keychain have all declined.
pub struct LegacyKeychainStore {
    inner: KeychainStore,
    /// Keys already warned about in this process.
    ///
    /// A poisoned lock here must not take down a credential
    /// lookup, so every use recovers from poisoning — the worst
    /// case is a duplicate warning.
    warned: Mutex<HashSet<String>>,
}

impl LegacyKeychainStore {
    /// Wrap the default keychain service.
    pub fn new() -> Self {
        Self::wrapping(KeychainStore::new())
    }

    /// Wrap a specific keychain store.
    ///
    /// Tests use this with a scoped service name so they never
    /// touch the developer's own credentials.
    pub fn wrapping(inner: KeychainStore) -> Self {
        Self {
            inner,
            warned: Mutex::new(HashSet::new()),
        }
    }

    /// Warn the first time a given key is served this way.
    ///
    /// Returns whether this call was the one that warned, which
    /// is what the tests assert on — "it warned" is the behaviour
    /// worth pinning, and capturing `tracing` output to prove it
    /// would test the subscriber more than this store.
    fn warn_once(&self, key: &str) -> bool {
        let first = match self.warned.lock() {
            Ok(mut seen) => seen.insert(key.to_owned()),
            Err(poisoned) => poisoned.into_inner().insert(key.to_owned()),
        };

        if first {
            warn!(
                key = key,
                "resolved '{key}' from the legacy OS keychain. The keychain is no longer part \
                 of the default chain and this fallback is removed in {FALLBACK_REMOVED_IN} — \
                 run `devboy secrets migrate --all` to move it into the local vault"
            );
        }
        first
    }
}

impl Default for LegacyKeychainStore {
    fn default() -> Self {
        Self::new()
    }
}

impl CredentialStore for LegacyKeychainStore {
    /// Always an error. The chain skips this store for writes
    /// anyway (see [`CredentialStore::is_writable`]); this is the
    /// answer for anyone who calls it directly.
    fn store(&self, key: &str, _value: &SecretString) -> Result<()> {
        Err(Error::Storage(format!(
            "refusing to write '{key}' into the OS keychain: it is read-only here, kept only so \
             secrets from before {removed} keep resolving. Store it with `devboy secrets ui`, \
             export the matching environment variable, or re-enable the keychain for writes with \
             `devboy config set secrets.keychain.enabled true`.",
            removed = FALLBACK_REMOVED_IN
        )))
    }

    fn get(&self, key: &str) -> Result<Option<SecretString>> {
        let found = self.inner.get(key)?;
        if found.is_some() {
            self.warn_once(key);
        }
        Ok(found)
    }

    /// Always an error.
    ///
    /// Deleting through the fallback would let a routine cleanup
    /// destroy the pre-upgrade copy of a secret that has not been
    /// migrated yet. `devboy secrets migrate` deletes from the
    /// keychain deliberately, addressing it directly.
    fn delete(&self, key: &str) -> Result<()> {
        Err(Error::Storage(format!(
            "refusing to delete '{key}' from the OS keychain through the legacy fallback: it is \
             read-only. `devboy secrets migrate {key}` moves the value out and removes it."
        )))
    }

    fn is_available(&self) -> bool {
        self.inner.is_available()
    }

    fn is_writable(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use secrecy::ExposeSecret;

    /// Every test gets its own keychain service so a run never
    /// reads or writes the developer's real credentials.
    fn scoped(name: &str) -> KeychainStore {
        KeychainStore::with_service_name(format!("devboy-tools-legacy-test-{name}"))
    }

    /// Guards the one property the whole store exists for. A
    /// version of this that also implemented `store` would pass
    /// every read test and quietly restore the dependency §6 set
    /// out to remove.
    #[test]
    fn writes_are_refused_and_say_where_to_put_it_instead() {
        let store = LegacyKeychainStore::wrapping(scoped("write-refused"));

        let err = store
            .store("personal/github/token", &SecretString::from("x".to_owned()))
            .expect_err("the legacy store must never accept a write");
        let msg = err.to_string();

        assert!(msg.contains("read-only"), "{msg}");
        assert!(
            msg.contains("devboy secrets ui") || msg.contains("environment variable"),
            "a refusal has to name somewhere the value can actually go: {msg}"
        );
        assert!(
            msg.contains(FALLBACK_REMOVED_IN),
            "the refusal should date itself: {msg}"
        );
    }

    #[test]
    fn deletes_are_refused_and_point_at_migrate() {
        let store = LegacyKeychainStore::wrapping(scoped("delete-refused"));

        let err = store
            .delete("personal/github/token")
            .expect_err("the legacy store must never accept a delete");

        assert!(err.to_string().contains("secrets migrate"), "{err}");
    }

    /// The chain consults `is_writable` before offering a write,
    /// so this is what actually keeps writes away in practice —
    /// the error above is the backstop for direct callers.
    #[test]
    fn the_chain_is_told_not_to_offer_writes() {
        assert!(!LegacyKeychainStore::wrapping(scoped("writable")).is_writable());
    }

    /// A miss must not warn: the store sits last in the chain and
    /// is asked about every secret that got that far, so warning
    /// on absence would fire constantly and mean nothing.
    #[test]
    fn a_key_that_is_not_there_warns_about_nothing() {
        let store = LegacyKeychainStore::wrapping(scoped("miss"));

        // The keychain backend may be absent in this environment;
        // either way the answer is "no value, no warning".
        let _ = store.get("personal/github/definitely-absent");

        assert!(
            store.warned.lock().unwrap().is_empty(),
            "an absent key produced a deprecation warning"
        );
    }

    /// Repeated resolution of the same secret warns once. Several
    /// identical lines per command read as noise, and noise gets
    /// filtered — which lands in the same place as never having
    /// warned at all.
    #[test]
    fn the_same_key_warns_once_and_a_second_key_warns_again() {
        let store = LegacyKeychainStore::wrapping(scoped("dedup"));

        assert!(store.warn_once("personal/github/token"));
        assert!(!store.warn_once("personal/github/token"));
        assert!(store.warn_once("personal/gitlab/token"));
    }

    /// Round-trips a real value through the OS keychain when the
    /// runner has one. Skipped rather than failed where it does
    /// not — a container without a keychain is a legitimate place
    /// to run the test suite, and this store is switched off
    /// there anyway.
    #[test]
    fn a_value_written_before_the_upgrade_still_resolves() {
        let inner = scoped("read-through");
        if !inner.is_available() {
            return;
        }
        let key = "personal/github/token-legacy-fallback";
        if inner
            .store(key, &SecretString::from("ghp_from_0_33".to_owned()))
            .is_err()
        {
            return; // Backend present but not writable in this environment.
        }

        let store = LegacyKeychainStore::wrapping(scoped("read-through"));
        let found = store.get(key).expect("read").expect("value is there");
        assert_eq!(found.expose_secret(), "ghp_from_0_33");
        assert!(
            store.warned.lock().unwrap().contains(key),
            "serving a legacy secret has to say so"
        );

        let _ = inner.delete(key);
    }
}
