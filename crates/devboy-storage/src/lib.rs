//! Secure credential storage using OS keychain.
//!
//! This crate provides secure storage for API tokens and credentials
//! using the operating system's native keychain/credential manager:
//!
//! - **macOS**: Keychain Services
//! - **Windows**: Credential Manager
//! - **Linux**: Secret Service (GNOME Keyring / KWallet)
//!
//! # Example
//!
//! ```ignore
//! use devboy_storage::{KeychainStore, CredentialStore};
//!
//! let store = KeychainStore::new();
//!
//! // Store a credential
//! store.store("gitlab/token", "glpat-xxx")?;
//!
//! // Retrieve it
//! let token = store.get("gitlab/token")?;
//! assert_eq!(token, Some("glpat-xxx".to_string()));
//!
//! // Delete when done
//! store.delete("gitlab/token")?;
//! ```

use devboy_core::{Error, Result};
use keyring::Entry;
use tracing::{debug, warn};

/// Service name used in OS keychain.
const SERVICE_NAME: &str = "devboy-tools";

/// Credential storage trait.
///
/// Implementations can use OS keychain, in-memory storage (for testing),
/// or other backends.
pub trait CredentialStore: Send + Sync {
    /// Store a credential securely.
    ///
    /// The key should follow the convention: `{provider}/{credential_name}`
    /// For example: `gitlab/token`, `github/token`, `jira/email`
    fn store(&self, key: &str, value: &str) -> Result<()>;

    /// Retrieve a stored credential.
    ///
    /// Returns `Ok(None)` if the credential doesn't exist.
    fn get(&self, key: &str) -> Result<Option<String>>;

    /// Delete a stored credential.
    ///
    /// Returns `Ok(())` even if the credential didn't exist.
    fn delete(&self, key: &str) -> Result<()>;

    /// Check if a credential exists.
    fn exists(&self, key: &str) -> bool {
        matches!(self.get(key), Ok(Some(_)))
    }
}

// =============================================================================
// KeychainStore - OS Keychain implementation
// =============================================================================

/// Credential store using the OS keychain.
///
/// This is the recommended store for production use. It securely stores
/// credentials in:
/// - macOS: Keychain Services
/// - Windows: Credential Manager
/// - Linux: Secret Service (GNOME Keyring / KWallet)
#[derive(Debug, Default)]
pub struct KeychainStore {
    service_name: String,
}

impl KeychainStore {
    /// Create a new keychain store with the default service name.
    pub fn new() -> Self {
        Self {
            service_name: SERVICE_NAME.to_string(),
        }
    }

    /// Create a keychain store with a custom service name.
    ///
    /// Useful for testing to avoid conflicts with real credentials.
    pub fn with_service_name(service_name: impl Into<String>) -> Self {
        Self {
            service_name: service_name.into(),
        }
    }

    fn make_entry(&self, key: &str) -> std::result::Result<Entry, keyring::Error> {
        Entry::new(&self.service_name, key)
    }
}

impl CredentialStore for KeychainStore {
    fn store(&self, key: &str, value: &str) -> Result<()> {
        debug!(key = key, "Storing credential in keychain");

        let entry = self.make_entry(key).map_err(|e| {
            Error::Storage(format!(
                "Failed to create keychain entry for '{}': {}",
                key, e
            ))
        })?;

        entry
            .set_password(value)
            .map_err(|e| Error::Storage(format!("Failed to store credential '{}': {}", key, e)))?;

        Ok(())
    }

    fn get(&self, key: &str) -> Result<Option<String>> {
        debug!(key = key, "Retrieving credential from keychain");

        let entry = self.make_entry(key).map_err(|e| {
            Error::Storage(format!(
                "Failed to create keychain entry for '{}': {}",
                key, e
            ))
        })?;

        match entry.get_password() {
            Ok(password) => Ok(Some(password)),
            Err(keyring::Error::NoEntry) => {
                debug!(key = key, "Credential not found");
                Ok(None)
            }
            Err(e) => {
                warn!(key = key, error = %e, "Failed to retrieve credential");
                Err(Error::Storage(format!(
                    "Failed to retrieve credential '{}': {}",
                    key, e
                )))
            }
        }
    }

    fn delete(&self, key: &str) -> Result<()> {
        debug!(key = key, "Deleting credential from keychain");

        let entry = self.make_entry(key).map_err(|e| {
            Error::Storage(format!(
                "Failed to create keychain entry for '{}': {}",
                key, e
            ))
        })?;

        match entry.delete_credential() {
            Ok(()) => Ok(()),
            Err(keyring::Error::NoEntry) => {
                // Already deleted, that's fine
                debug!(key = key, "Credential was already deleted");
                Ok(())
            }
            Err(e) => Err(Error::Storage(format!(
                "Failed to delete credential '{}': {}",
                key, e
            ))),
        }
    }
}

// =============================================================================
// MemoryStore - In-memory implementation for testing
// =============================================================================

/// In-memory credential store for testing.
///
/// This store keeps credentials in memory and is suitable for unit tests
/// where you don't want to interact with the real OS keychain.
#[derive(Debug, Default)]
pub struct MemoryStore {
    credentials: std::sync::RwLock<std::collections::HashMap<String, String>>,
}

impl MemoryStore {
    /// Create a new in-memory store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a store pre-populated with credentials.
    pub fn with_credentials(credentials: impl IntoIterator<Item = (String, String)>) -> Self {
        let store = Self::new();
        {
            let mut creds = store.credentials.write().unwrap();
            creds.extend(credentials);
        }
        store
    }
}

impl CredentialStore for MemoryStore {
    fn store(&self, key: &str, value: &str) -> Result<()> {
        let mut creds = self
            .credentials
            .write()
            .map_err(|e| Error::Storage(format!("Lock poisoned: {}", e)))?;
        creds.insert(key.to_string(), value.to_string());
        Ok(())
    }

    fn get(&self, key: &str) -> Result<Option<String>> {
        let creds = self
            .credentials
            .read()
            .map_err(|e| Error::Storage(format!("Lock poisoned: {}", e)))?;
        Ok(creds.get(key).cloned())
    }

    fn delete(&self, key: &str) -> Result<()> {
        let mut creds = self
            .credentials
            .write()
            .map_err(|e| Error::Storage(format!("Lock poisoned: {}", e)))?;
        creds.remove(key);
        Ok(())
    }
}

// =============================================================================
// Helper functions
// =============================================================================

/// Standard credential key for a provider's API token.
pub fn token_key(provider: &str) -> String {
    format!("{}/token", provider)
}

/// Standard credential key for a provider's email (used by Jira).
pub fn email_key(provider: &str) -> String {
    format!("{}/email", provider)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_memory_store_basic() {
        let store = MemoryStore::new();

        // Store
        store.store("test/key", "test-value").unwrap();

        // Get
        let value = store.get("test/key").unwrap();
        assert_eq!(value, Some("test-value".to_string()));

        // Exists
        assert!(store.exists("test/key"));
        assert!(!store.exists("nonexistent"));

        // Delete
        store.delete("test/key").unwrap();
        let value = store.get("test/key").unwrap();
        assert_eq!(value, None);

        // Delete non-existent (should not error)
        store.delete("nonexistent").unwrap();
    }

    #[test]
    fn test_memory_store_with_credentials() {
        let store = MemoryStore::with_credentials([
            ("gitlab/token".to_string(), "glpat-xxx".to_string()),
            ("github/token".to_string(), "ghp-yyy".to_string()),
        ]);

        assert_eq!(
            store.get("gitlab/token").unwrap(),
            Some("glpat-xxx".to_string())
        );
        assert_eq!(
            store.get("github/token").unwrap(),
            Some("ghp-yyy".to_string())
        );
    }

    #[test]
    fn test_token_key() {
        assert_eq!(token_key("gitlab"), "gitlab/token");
        assert_eq!(token_key("github"), "github/token");
    }

    #[test]
    fn test_email_key() {
        assert_eq!(email_key("jira"), "jira/email");
    }

    // Note: KeychainStore tests are not included here because they would
    // interact with the real OS keychain. Integration tests for KeychainStore
    // should be run separately with appropriate cleanup.
}
