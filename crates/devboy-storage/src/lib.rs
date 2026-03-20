//! Secure credential storage with multiple backends.
//!
//! This crate provides credential storage with support for:
//!
//! - **OS Keychain**: macOS Keychain, Windows Credential Manager, Linux Secret Service
//! - **Environment Variables**: For CI/CD and containerized environments
//! - **Chain Store**: Composable fallback between multiple backends
//!
//! # Credential Resolution Order
//!
//! When using `ChainStore::default_chain()`, credentials are resolved in this order:
//!
//! 1. **Environment variables** (highest priority, for CI/CD)
//!    - `DEVBOY_{PROVIDER}_TOKEN` (e.g., `DEVBOY_GITHUB_TOKEN`)
//!    - `{PROVIDER}_TOKEN` (fallback, e.g., `GITHUB_TOKEN`)
//! 2. **OS Keychain** (for local development)
//!
//! # Example
//!
//! ```ignore
//! use devboy_storage::{ChainStore, CredentialStore};
//!
//! // Use the default chain (env vars -> keychain)
//! let store = ChainStore::default_chain();
//!
//! // This will check DEVBOY_GITHUB_TOKEN, then GITHUB_TOKEN,
//! // then keychain for "github.token"
//! let token = store.get("github.token")?;
//!
//! // Or use keychain directly for local development
//! use devboy_storage::KeychainStore;
//! let keychain = KeychainStore::new();
//! keychain.store("gitlab.token", "glpat-xxx")?;
//! ```

use devboy_core::{Error, Result};
use keyring::Entry;
use tracing::{debug, warn};

/// Service name used in OS keychain.
const SERVICE_NAME: &str = "devboy-tools";

/// Credential storage trait.
///
/// Implementations can use OS keychain, environment variables, in-memory storage,
/// or other backends.
pub trait CredentialStore: Send + Sync {
    /// Store a credential securely.
    ///
    /// The key should follow the convention: `{provider}.{credential_name}`
    /// For example: `gitlab.token`, `github.token`, `jira.email`
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

    /// Check if this credential store is available and functional.
    ///
    /// Returns `true` if the store can be used for credential operations.
    /// This is useful for checking keychain availability in CI/container environments.
    fn is_available(&self) -> bool {
        true
    }

    /// Check if this store supports write operations.
    ///
    /// Some stores (like `EnvVarStore`) are read-only.
    fn is_writable(&self) -> bool {
        true
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
#[derive(Debug)]
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

impl Default for KeychainStore {
    fn default() -> Self {
        Self::new()
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

    fn is_available(&self) -> bool {
        // Try to create an entry - if this fails, keychain is not available
        // We don't actually write anything, just check if the backend is functional
        match self.make_entry("__devboy_availability_check__") {
            Ok(_) => true,
            Err(e) => {
                debug!(error = %e, "Keychain not available");
                false
            }
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
// EnvVarStore - Environment variable implementation for CI/CD
// =============================================================================

/// Default prefix for environment variables.
const DEFAULT_ENV_PREFIX: &str = "DEVBOY";

/// Credential store using environment variables.
///
/// This is a read-only store that reads credentials from environment variables.
/// It's designed for CI/CD pipelines and containerized environments where
/// OS keychain may not be available.
///
/// # Key to Environment Variable Mapping
///
/// The key is converted to an environment variable name:
/// - Converted to uppercase
/// - Dots (`.`) and slashes (`/`) replaced with underscores (`_`)
/// - Prefixed with `DEVBOY_` by default
///
/// Examples:
/// - `github.token` → `DEVBOY_GITHUB_TOKEN` (then `GITHUB_TOKEN` as fallback)
/// - `contexts.dashboard.github.token` → `DEVBOY_CONTEXTS_DASHBOARD_GITHUB_TOKEN`
///
/// # Example
///
/// ```ignore
/// use devboy_storage::{EnvVarStore, CredentialStore};
///
/// // Set environment variable before running
/// std::env::set_var("DEVBOY_GITHUB_TOKEN", "ghp_xxx");
///
/// let store = EnvVarStore::new();
/// let token = store.get("github.token")?;
/// assert_eq!(token, Some("ghp_xxx".to_string()));
/// ```
#[derive(Debug)]
pub struct EnvVarStore {
    /// Prefix for environment variables (e.g., "DEVBOY").
    prefix: String,
    /// Whether to fall back to unprefixed variable names.
    fallback_without_prefix: bool,
}

impl EnvVarStore {
    /// Create a new environment variable store with default settings.
    ///
    /// Uses `DEVBOY_` prefix and enables fallback to unprefixed variables.
    pub fn new() -> Self {
        Self {
            prefix: DEFAULT_ENV_PREFIX.to_string(),
            fallback_without_prefix: true,
        }
    }

    /// Create with a custom prefix.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let store = EnvVarStore::with_prefix("MYAPP");
    /// // Will check MYAPP_GITHUB_TOKEN, then GITHUB_TOKEN
    /// ```
    pub fn with_prefix(prefix: impl Into<String>) -> Self {
        Self {
            prefix: prefix.into(),
            fallback_without_prefix: true,
        }
    }

    /// Disable fallback to unprefixed environment variables.
    ///
    /// When disabled, only `{PREFIX}_{KEY}` format is checked.
    pub fn without_fallback(mut self) -> Self {
        self.fallback_without_prefix = false;
        self
    }

    /// Convert a credential key to environment variable name.
    ///
    /// - Uppercase the key
    /// - Replace `.`, `/`, and `-` with `_`
    fn key_to_env_name(&self, key: &str) -> String {
        key.to_uppercase().replace(['.', '/', '-'], "_")
    }

    /// Get the prefixed environment variable name.
    fn prefixed_env_name(&self, key: &str) -> String {
        format!("{}_{}", self.prefix, self.key_to_env_name(key))
    }

    /// Get the unprefixed environment variable name.
    fn unprefixed_env_name(&self, key: &str) -> String {
        self.key_to_env_name(key)
    }
}

impl Default for EnvVarStore {
    fn default() -> Self {
        Self::new()
    }
}

impl CredentialStore for EnvVarStore {
    fn store(&self, _key: &str, _value: &str) -> Result<()> {
        Err(Error::Storage(
            "EnvVarStore is read-only. Use OS keychain or set environment variables directly."
                .to_string(),
        ))
    }

    fn get(&self, key: &str) -> Result<Option<String>> {
        // Try prefixed first (e.g., DEVBOY_GITHUB_TOKEN)
        let prefixed = self.prefixed_env_name(key);
        if let Ok(value) = std::env::var(&prefixed) {
            debug!(key = key, env_var = %prefixed, "Found credential in environment variable");
            return Ok(Some(value));
        }

        // Fallback to unprefixed (e.g., GITHUB_TOKEN)
        if self.fallback_without_prefix {
            let unprefixed = self.unprefixed_env_name(key);
            if let Ok(value) = std::env::var(&unprefixed) {
                debug!(key = key, env_var = %unprefixed, "Found credential in environment variable (unprefixed)");
                return Ok(Some(value));
            }
        }

        debug!(key = key, "Credential not found in environment variables");
        Ok(None)
    }

    fn delete(&self, _key: &str) -> Result<()> {
        Err(Error::Storage(
            "EnvVarStore is read-only. Environment variables cannot be deleted.".to_string(),
        ))
    }

    fn is_writable(&self) -> bool {
        false
    }
}

// =============================================================================
// ChainStore - Composable credential store with fallback
// =============================================================================

/// Composable credential store that chains multiple backends.
///
/// Attempts to retrieve credentials from each store in order until one succeeds.
/// Write operations go to the first writable store.
///
/// # Default Chain
///
/// Use `ChainStore::default_chain()` for the recommended configuration:
/// 1. Environment variables (highest priority, for CI/CD)
/// 2. OS Keychain (for local development)
///
/// # Example
///
/// ```ignore
/// use devboy_storage::{ChainStore, CredentialStore};
///
/// // Use default chain
/// let store = ChainStore::default_chain();
///
/// // Or create custom chain
/// use devboy_storage::{EnvVarStore, MemoryStore};
/// let store = ChainStore::new(vec![
///     Box::new(EnvVarStore::new()),
///     Box::new(MemoryStore::new()),
/// ]);
/// ```
pub struct ChainStore {
    stores: Vec<Box<dyn CredentialStore>>,
}

impl ChainStore {
    /// Create a chain store from a list of stores.
    ///
    /// Stores are tried in order for read operations.
    /// The first writable store is used for write operations.
    pub fn new(stores: Vec<Box<dyn CredentialStore>>) -> Self {
        Self { stores }
    }

    /// Create the default credential chain.
    ///
    /// Order:
    /// 1. Environment variables (`EnvVarStore`)
    /// 2. OS Keychain (`KeychainStore`)
    ///
    /// This is the recommended configuration for most use cases:
    /// - CI/CD can set `DEVBOY_*` or provider-specific env vars
    /// - Local development uses keychain transparently
    pub fn default_chain() -> Self {
        Self::new(vec![
            Box::new(EnvVarStore::new()),
            Box::new(KeychainStore::new()),
        ])
    }

    /// Create a chain for CI/CD environments (no keychain).
    ///
    /// Only uses environment variables and memory store.
    /// Useful when keychain is not available.
    pub fn ci_chain() -> Self {
        Self::new(vec![
            Box::new(EnvVarStore::new()),
            Box::new(MemoryStore::new()),
        ])
    }

    /// Get the number of stores in the chain.
    pub fn len(&self) -> usize {
        self.stores.len()
    }

    /// Check if the chain is empty.
    pub fn is_empty(&self) -> bool {
        self.stores.is_empty()
    }
}

impl std::fmt::Debug for ChainStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChainStore")
            .field("stores_count", &self.stores.len())
            .finish()
    }
}

impl CredentialStore for ChainStore {
    fn store(&self, key: &str, value: &str) -> Result<()> {
        // Find first writable store
        for store in &self.stores {
            if store.is_writable() {
                return store.store(key, value);
            }
        }
        Err(Error::Storage(
            "No writable credential store available in chain".to_string(),
        ))
    }

    fn get(&self, key: &str) -> Result<Option<String>> {
        // Try each store in order
        for store in &self.stores {
            match store.get(key) {
                Ok(Some(value)) => return Ok(Some(value)),
                Ok(None) => continue,
                Err(e) => {
                    // Log error but continue to next store
                    debug!(key = key, error = %e, "Store returned error, trying next");
                    continue;
                }
            }
        }
        Ok(None)
    }

    fn delete(&self, key: &str) -> Result<()> {
        // Delete from all writable stores
        let mut deleted_any = false;
        let mut last_error: Option<Error> = None;

        for store in &self.stores {
            if store.is_writable() {
                match store.delete(key) {
                    Ok(()) => deleted_any = true,
                    Err(e) => last_error = Some(e),
                }
            }
        }

        if deleted_any {
            Ok(())
        } else if let Some(e) = last_error {
            Err(e)
        } else {
            // No writable stores, but that's ok for delete
            Ok(())
        }
    }

    fn is_available(&self) -> bool {
        // Available if at least one store is available
        self.stores.iter().any(|s| s.is_available())
    }

    fn is_writable(&self) -> bool {
        // Writable if at least one store is writable
        self.stores.iter().any(|s| s.is_writable())
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

    #[test]
    fn test_memory_store_delete_nonexistent() {
        let store = MemoryStore::new();

        // Delete non-existent key should succeed
        store.delete("nonexistent/key").unwrap();

        // Verify it's still not there
        assert_eq!(store.get("nonexistent/key").unwrap(), None);
    }

    #[test]
    fn test_memory_store_exists() {
        let store = MemoryStore::new();

        assert!(!store.exists("test/key"));

        store.store("test/key", "value").unwrap();
        assert!(store.exists("test/key"));

        store.delete("test/key").unwrap();
        assert!(!store.exists("test/key"));
    }

    #[test]
    fn test_memory_store_overwrite() {
        let store = MemoryStore::new();

        store.store("test/key", "value1").unwrap();
        assert_eq!(store.get("test/key").unwrap(), Some("value1".to_string()));

        store.store("test/key", "value2").unwrap();
        assert_eq!(store.get("test/key").unwrap(), Some("value2".to_string()));
    }

    #[test]
    fn test_credential_store_exists_default_impl() {
        // Test the default exists() impl from the trait
        let store = MemoryStore::new();

        store.store("key1", "val1").unwrap();

        // CredentialStore::exists uses the default impl calling get()
        assert!(CredentialStore::exists(&store, "key1"));
        assert!(!CredentialStore::exists(&store, "key2"));
    }

    #[test]
    fn test_keychain_store_new() {
        let store = KeychainStore::new();
        assert_eq!(store.service_name, "devboy-tools");
    }

    #[test]
    fn test_keychain_store_with_service_name() {
        let store = KeychainStore::with_service_name("test-service");
        assert_eq!(store.service_name, "test-service");
    }

    #[test]
    fn test_keychain_store_default() {
        let store = KeychainStore::default();
        assert_eq!(store.service_name, "devboy-tools");
    }

    // Note: KeychainStore tests are not included here because they would
    // interact with the real OS keychain. Integration tests for KeychainStore
    // should be run separately with appropriate cleanup.

    // =========================================================================
    // EnvVarStore tests
    // =========================================================================

    #[test]
    fn test_env_var_store_new() {
        let store = EnvVarStore::new();
        assert_eq!(store.prefix, "DEVBOY");
        assert!(store.fallback_without_prefix);
    }

    #[test]
    fn test_env_var_store_with_prefix() {
        let store = EnvVarStore::with_prefix("CUSTOM");
        assert_eq!(store.prefix, "CUSTOM");
        assert!(store.fallback_without_prefix);
    }

    #[test]
    fn test_env_var_store_without_fallback() {
        let store = EnvVarStore::new().without_fallback();
        assert!(!store.fallback_without_prefix);
    }

    #[test]
    fn test_env_var_store_key_to_env_name() {
        let store = EnvVarStore::new();

        // Test various key formats
        assert_eq!(store.key_to_env_name("github.token"), "GITHUB_TOKEN");
        assert_eq!(store.key_to_env_name("gitlab/token"), "GITLAB_TOKEN");
        assert_eq!(
            store.key_to_env_name("contexts.dashboard.github.token"),
            "CONTEXTS_DASHBOARD_GITHUB_TOKEN"
        );
        // Dashes should also be converted
        assert_eq!(
            store.key_to_env_name("devboy-cloud.token"),
            "DEVBOY_CLOUD_TOKEN"
        );
    }

    #[test]
    fn test_env_var_store_prefixed_env_name() {
        let store = EnvVarStore::new();
        assert_eq!(
            store.prefixed_env_name("github.token"),
            "DEVBOY_GITHUB_TOKEN"
        );

        let custom = EnvVarStore::with_prefix("MYAPP");
        assert_eq!(
            custom.prefixed_env_name("github.token"),
            "MYAPP_GITHUB_TOKEN"
        );
    }

    #[test]
    fn test_env_var_store_get_prefixed() {
        let store = EnvVarStore::new();

        // Set prefixed env var
        std::env::set_var("DEVBOY_TEST_TOKEN", "prefixed-value");

        let result = store.get("test.token").unwrap();
        assert_eq!(result, Some("prefixed-value".to_string()));

        // Cleanup
        std::env::remove_var("DEVBOY_TEST_TOKEN");
    }

    #[test]
    fn test_env_var_store_get_unprefixed_fallback() {
        let store = EnvVarStore::new();

        // Set only unprefixed env var
        std::env::set_var("TEST_FALLBACK_TOKEN", "unprefixed-value");

        let result = store.get("test.fallback.token").unwrap();
        assert_eq!(result, Some("unprefixed-value".to_string()));

        // Cleanup
        std::env::remove_var("TEST_FALLBACK_TOKEN");
    }

    #[test]
    fn test_env_var_store_prefixed_takes_priority() {
        let store = EnvVarStore::new();

        // Set both prefixed and unprefixed
        std::env::set_var("DEVBOY_TEST_PRIORITY_TOKEN", "prefixed");
        std::env::set_var("TEST_PRIORITY_TOKEN", "unprefixed");

        let result = store.get("test.priority.token").unwrap();
        assert_eq!(result, Some("prefixed".to_string()));

        // Cleanup
        std::env::remove_var("DEVBOY_TEST_PRIORITY_TOKEN");
        std::env::remove_var("TEST_PRIORITY_TOKEN");
    }

    #[test]
    fn test_env_var_store_no_fallback() {
        let store = EnvVarStore::new().without_fallback();

        // Set only unprefixed env var
        std::env::set_var("TEST_NO_FALLBACK_TOKEN", "unprefixed-value");

        // Should NOT find it because fallback is disabled
        let result = store.get("test.no.fallback.token").unwrap();
        assert_eq!(result, None);

        // Cleanup
        std::env::remove_var("TEST_NO_FALLBACK_TOKEN");
    }

    #[test]
    fn test_env_var_store_not_found() {
        let store = EnvVarStore::new();

        let result = store.get("nonexistent.key.that.does.not.exist").unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn test_env_var_store_is_read_only() {
        let store = EnvVarStore::new();

        assert!(!store.is_writable());

        let store_result = store.store("test.key", "value");
        assert!(store_result.is_err());

        let delete_result = store.delete("test.key");
        assert!(delete_result.is_err());
    }

    #[test]
    fn test_env_var_store_default() {
        let store = EnvVarStore::default();
        assert_eq!(store.prefix, "DEVBOY");
    }

    // =========================================================================
    // ChainStore tests
    // =========================================================================

    #[test]
    fn test_chain_store_new() {
        let store = ChainStore::new(vec![]);
        assert!(store.is_empty());
        assert_eq!(store.len(), 0);
    }

    #[test]
    fn test_chain_store_default_chain() {
        let store = ChainStore::default_chain();
        assert_eq!(store.len(), 2); // EnvVarStore + KeychainStore
        assert!(!store.is_empty());
    }

    #[test]
    fn test_chain_store_ci_chain() {
        let store = ChainStore::ci_chain();
        assert_eq!(store.len(), 2); // EnvVarStore + MemoryStore
    }

    #[test]
    fn test_chain_store_get_first_match_wins() {
        // Create chain with two memory stores
        let store1 = MemoryStore::with_credentials([("key1".to_string(), "value1".to_string())]);
        let store2 = MemoryStore::with_credentials([
            ("key1".to_string(), "value2".to_string()),
            ("key2".to_string(), "value2".to_string()),
        ]);

        let chain = ChainStore::new(vec![Box::new(store1), Box::new(store2)]);

        // key1 should come from first store
        assert_eq!(chain.get("key1").unwrap(), Some("value1".to_string()));

        // key2 should come from second store (not in first)
        assert_eq!(chain.get("key2").unwrap(), Some("value2".to_string()));

        // key3 not found in either
        assert_eq!(chain.get("key3").unwrap(), None);
    }

    #[test]
    fn test_chain_store_store_to_first_writable() {
        // EnvVarStore (read-only) + MemoryStore (writable)
        let chain = ChainStore::new(vec![
            Box::new(EnvVarStore::new()),
            Box::new(MemoryStore::new()),
        ]);

        // Should store to MemoryStore (first writable)
        chain.store("test.key", "test-value").unwrap();

        // Should retrieve from chain
        assert_eq!(
            chain.get("test.key").unwrap(),
            Some("test-value".to_string())
        );
    }

    #[test]
    fn test_chain_store_no_writable_store_error() {
        // Chain with only read-only stores
        let chain = ChainStore::new(vec![Box::new(EnvVarStore::new())]);

        let result = chain.store("test.key", "value");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("No writable"));
    }

    #[test]
    fn test_chain_store_delete_from_all_writable() {
        let store1 = MemoryStore::new();
        let store2 = MemoryStore::new();

        // Store in both
        store1.store("key", "val1").unwrap();
        store2.store("key", "val2").unwrap();

        let chain = ChainStore::new(vec![Box::new(store1), Box::new(store2)]);

        // Delete should remove from both
        chain.delete("key").unwrap();

        // Neither should have the key now
        assert_eq!(chain.get("key").unwrap(), None);
    }

    #[test]
    fn test_chain_store_is_available() {
        // Empty chain
        let empty = ChainStore::new(vec![]);
        assert!(!empty.is_available());

        // Chain with available store
        let with_memory = ChainStore::new(vec![Box::new(MemoryStore::new())]);
        assert!(with_memory.is_available());
    }

    #[test]
    fn test_chain_store_is_writable() {
        // Read-only chain
        let read_only = ChainStore::new(vec![Box::new(EnvVarStore::new())]);
        assert!(!read_only.is_writable());

        // Writable chain
        let writable = ChainStore::new(vec![Box::new(MemoryStore::new())]);
        assert!(writable.is_writable());
    }

    #[test]
    fn test_chain_store_env_var_priority() {
        // This tests the real use case: env var takes priority over memory

        // Set up env var
        std::env::set_var("DEVBOY_CHAIN_TEST_TOKEN", "from-env");

        // Set up memory store with different value
        let memory = MemoryStore::with_credentials([(
            "chain.test.token".to_string(),
            "from-memory".to_string(),
        )]);

        // Chain: env -> memory
        let chain = ChainStore::new(vec![Box::new(EnvVarStore::new()), Box::new(memory)]);

        // Env var should win
        assert_eq!(
            chain.get("chain.test.token").unwrap(),
            Some("from-env".to_string())
        );

        // Cleanup
        std::env::remove_var("DEVBOY_CHAIN_TEST_TOKEN");
    }

    #[test]
    fn test_chain_store_fallback_to_memory_when_env_empty() {
        // Memory store with value
        let memory = MemoryStore::with_credentials([(
            "fallback.test.token".to_string(),
            "from-memory".to_string(),
        )]);

        // Chain: env (empty) -> memory
        let chain = ChainStore::new(vec![Box::new(EnvVarStore::new()), Box::new(memory)]);

        // Should fall back to memory
        assert_eq!(
            chain.get("fallback.test.token").unwrap(),
            Some("from-memory".to_string())
        );
    }

    #[test]
    fn test_chain_store_debug_impl() {
        let chain = ChainStore::default_chain();
        let debug_str = format!("{:?}", chain);
        assert!(debug_str.contains("ChainStore"));
        assert!(debug_str.contains("stores_count"));
    }
}
