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
use secrecy::{ExposeSecret, SecretString};
use tracing::{debug, warn};

pub mod cache;
pub mod ci;
pub mod expiry;
pub mod index;
pub mod manifest;
pub mod merge;
pub mod pattern_resolution;
pub mod plugin_client;
pub mod plugin_manifest;
pub mod plugin_protocol;
pub mod router_cache;
pub mod router_config;
pub mod router_credentials;
pub mod router_resolve;
pub mod secret_path;
pub mod source;
pub mod validation;

pub use cache::CachedStore;
pub use ci::{
    CI_HEURISTIC_VARS, CiActivation, CiDetection, CiPolicy, DEVBOY_CI_ENV, detect_ci_mode,
};
pub use expiry::{ExpiryWarning, ExpiryWarningKind, WARNING_WINDOW_DAYS, check_rotation_reminders};
pub use index::{Gate, GlobalIndex, IndexEntry, IndexError, RotationMethod};
pub use manifest::{
    MANIFEST_RELATIVE_PATH, ManifestError, OverrideEntry, PathRole, ProjectManifest,
};
pub use merge::{
    MergeError, MergeOutput, MergeWarning, MergeWarningKind, OverrideField, ResolvedSecret,
    SecretOrigin, merge_manifest,
};
pub use pattern_resolution::{
    InheritanceWarning, InheritanceWarningKind, apply_pattern_inheritance,
};
pub use router_cache::{AdaptiveCache, CacheClock, DEFAULT_BASE_TTL, ManualClock, SystemClock};
pub use router_config::{
    DefaultRoute, RouteRule, RouterConfig, RouterConfigError, SOURCES_FILENAME, SecretOverride,
    SourceDefinition,
};
pub use router_credentials::{
    CredentialGraphError, SOURCE_CREDENTIALS_PREFIX, validate_source_credentials,
};
pub use router_resolve::{PathResolver, ResolveError, RouteDecision};
pub use secret_path::{PathError, SecretPath};
pub use source::{
    Capabilities, CredentialRef, GetOutcome, RemoteRef, SecretSource, SourceError, SourceStatus,
};
pub use validation::{FormatCheck, FormatRuleSource, validate_format};

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
    /// For example: `gitlab.token`, `github.token`, `jira.email`.
    ///
    /// The value is taken as `&SecretString` so callers cannot accidentally
    /// log or otherwise leak the plaintext on its way into storage.
    fn store(&self, key: &str, value: &SecretString) -> Result<()>;

    /// Retrieve a stored credential.
    ///
    /// Returns `Ok(None)` if the credential doesn't exist. The returned
    /// `SecretString` redacts itself in `Debug` output and zeroizes the
    /// buffer on drop — call `.expose_secret()` only at the boundary that
    /// actually consumes the secret (HTTP header, etc.).
    fn get(&self, key: &str) -> Result<Option<SecretString>>;

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
    fn store(&self, key: &str, value: &SecretString) -> Result<()> {
        debug!(key = key, "Storing credential in keychain");

        let entry = self.make_entry(key).map_err(|e| {
            Error::Storage(format!(
                "Failed to create keychain entry for '{}': {}",
                key, e
            ))
        })?;

        entry
            .set_password(value.expose_secret())
            .map_err(|e| Error::Storage(format!("Failed to store credential '{}': {}", key, e)))?;

        Ok(())
    }

    fn get(&self, key: &str) -> Result<Option<SecretString>> {
        debug!(key = key, "Retrieving credential from keychain");

        let entry = self.make_entry(key).map_err(|e| {
            Error::Storage(format!(
                "Failed to create keychain entry for '{}': {}",
                key, e
            ))
        })?;

        match entry.get_password() {
            Ok(password) => Ok(Some(SecretString::from(password))),
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
/// This store keeps credentials in memory wrapped in [`SecretString`]
/// (zeroize-on-drop, redacted `Debug`) for unit tests that don't want
/// to interact with the real OS keychain. The `Debug` impl shows the
/// key set and a count, never the values, so accidentally logging a
/// `MemoryStore` cannot leak plaintext.
#[derive(Default)]
pub struct MemoryStore {
    credentials: std::sync::RwLock<std::collections::HashMap<String, SecretString>>,
}

impl std::fmt::Debug for MemoryStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let creds = self.credentials.read();
        let (count, keys) = match &creds {
            Ok(map) => (map.len(), map.keys().cloned().collect::<Vec<_>>()),
            Err(_) => (0, vec!["<lock-poisoned>".to_string()]),
        };
        f.debug_struct("MemoryStore")
            .field("credentials", &format!("<{count} redacted secret(s)>"))
            .field("keys", &keys)
            .finish()
    }
}

impl MemoryStore {
    /// Create a new in-memory store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a store pre-populated with credentials. Accepts plaintext
    /// `(key, value)` pairs for test ergonomics; the values are wrapped
    /// in [`SecretString`] before storage.
    pub fn with_credentials(credentials: impl IntoIterator<Item = (String, String)>) -> Self {
        let store = Self::new();
        {
            let mut creds = store.credentials.write().unwrap();
            for (k, v) in credentials {
                creds.insert(k, SecretString::from(v));
            }
        }
        store
    }
}

impl CredentialStore for MemoryStore {
    fn store(&self, key: &str, value: &SecretString) -> Result<()> {
        let mut creds = self
            .credentials
            .write()
            .map_err(|e| Error::Storage(format!("Lock poisoned: {}", e)))?;
        // Clone the SecretString directly — no `expose_secret()` call,
        // no extra plaintext String allocation, and the cached value
        // keeps the same zeroize-on-drop discipline as the input.
        creds.insert(key.to_string(), value.clone());
        Ok(())
    }

    fn get(&self, key: &str) -> Result<Option<SecretString>> {
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
/// - Dots (`.`), slashes (`/`), and dashes (`-`) replaced with underscores (`_`)
/// - Prefixed with `DEVBOY_` by default
///
/// Examples:
/// - `github.token` → `DEVBOY_GITHUB_TOKEN` (then `GITHUB_TOKEN` as fallback)
/// - `contexts.dashboard.github.token` → `DEVBOY_CONTEXTS_DASHBOARD_GITHUB_TOKEN`
/// - `devboy-cloud.token` → `DEVBOY_DEVBOY_CLOUD_TOKEN`
///
/// # Example
///
/// ```ignore
/// use devboy_storage::{EnvVarStore, CredentialStore};
///
/// // Reads from DEVBOY_GITHUB_TOKEN env var (if set)
/// let store = EnvVarStore::new();
/// let token = store.get("github.token")?;
/// ```
/// Function type for reading environment variables.
/// Defaults to `std::env::var`, but can be replaced in tests.
type EnvReader = fn(&str) -> std::result::Result<String, std::env::VarError>;

/// Wrapper around `std::env::var` matching the `EnvReader` signature.
fn read_env_var(key: &str) -> std::result::Result<String, std::env::VarError> {
    std::env::var(key)
}

pub struct EnvVarStore {
    /// Prefix for environment variables (e.g., "DEVBOY").
    prefix: String,
    /// Whether to fall back to unprefixed variable names.
    fallback_without_prefix: bool,
    /// Function to read environment variables (injectable for testing).
    env_reader: EnvReader,
}

impl EnvVarStore {
    /// Create a new environment variable store with default settings.
    ///
    /// Uses `DEVBOY_` prefix and enables fallback to unprefixed variables.
    pub fn new() -> Self {
        Self {
            prefix: DEFAULT_ENV_PREFIX.to_string(),
            fallback_without_prefix: true,
            env_reader: read_env_var,
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
            env_reader: read_env_var,
        }
    }

    /// Disable fallback to unprefixed environment variables.
    ///
    /// When disabled, only `{PREFIX}_{KEY}` format is checked.
    pub fn without_fallback(mut self) -> Self {
        self.fallback_without_prefix = false;
        self
    }

    /// Replace the environment variable reader (for testing).
    #[cfg(test)]
    fn with_env_reader(mut self, reader: EnvReader) -> Self {
        self.env_reader = reader;
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

impl std::fmt::Debug for EnvVarStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EnvVarStore")
            .field("prefix", &self.prefix)
            .field("fallback_without_prefix", &self.fallback_without_prefix)
            .finish()
    }
}

impl Default for EnvVarStore {
    fn default() -> Self {
        Self::new()
    }
}

impl CredentialStore for EnvVarStore {
    fn store(&self, _key: &str, _value: &SecretString) -> Result<()> {
        Err(Error::Storage(
            "EnvVarStore is read-only. Use OS keychain or set environment variables directly."
                .to_string(),
        ))
    }

    fn get(&self, key: &str) -> Result<Option<SecretString>> {
        // Try prefixed first (e.g., DEVBOY_GITHUB_TOKEN)
        let prefixed = self.prefixed_env_name(key);
        if let Ok(value) = (self.env_reader)(&prefixed) {
            debug!(key = key, env_var = %prefixed, "Found credential in environment variable");
            return Ok(Some(SecretString::from(value)));
        }

        // Fallback to unprefixed (e.g., GITHUB_TOKEN)
        if self.fallback_without_prefix {
            let unprefixed = self.unprefixed_env_name(key);
            if let Ok(value) = (self.env_reader)(&unprefixed) {
                debug!(key = key, env_var = %unprefixed, "Found credential in environment variable (unprefixed)");
                return Ok(Some(SecretString::from(value)));
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
    fn store(&self, key: &str, value: &SecretString) -> Result<()> {
        // Try each writable and available store in order
        let mut last_error: Option<Error> = None;
        for store in &self.stores {
            if store.is_writable() && store.is_available() {
                match store.store(key, value) {
                    Ok(()) => return Ok(()),
                    Err(e) => {
                        debug!(key = key, error = %e, "Store write failed, trying next");
                        last_error = Some(e);
                    }
                }
            }
        }
        Err(last_error.unwrap_or_else(|| {
            Error::Storage("No writable credential store available in chain".to_string())
        }))
    }

    fn get(&self, key: &str) -> Result<Option<SecretString>> {
        // Try each store in order, tracking errors
        let mut last_error: Option<Error> = None;
        for store in &self.stores {
            match store.get(key) {
                Ok(Some(value)) => return Ok(Some(value)),
                Ok(None) => continue,
                Err(e) => {
                    // Log error but continue to next store
                    debug!(key = key, error = %e, "Store returned error, trying next");
                    last_error = Some(e);
                }
            }
        }
        // If all stores returned errors, propagate the last one
        if let Some(e) = last_error {
            Err(e)
        } else {
            Ok(None)
        }
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

/// Build the default credential chain, optionally wrapping the whole thing in a TTL
/// cache. Call this from host binaries (CLI, MCP server entrypoint) so the cache
/// configuration stays consistent.
///
/// - `cache_ttl_secs == 0` → no cache, returns the raw [`ChainStore`].
/// - `cache_ttl_secs > 0` → wraps in [`CachedStore`] with the requested TTL.
pub fn build_default_store(cache_ttl_secs: u64) -> Box<dyn CredentialStore> {
    let chain = ChainStore::default_chain();
    if cache_ttl_secs == 0 {
        Box::new(chain)
    } else {
        Box::new(CachedStore::new(
            chain,
            std::time::Duration::from_secs(cache_ttl_secs),
        ))
    }
}

/// Build a store on top of a user-provided backend (mainly useful for CI variants or
/// custom test harnesses). Same cache semantics as [`build_default_store`].
pub fn wrap_with_cache<S: CredentialStore + 'static>(
    inner: S,
    cache_ttl_secs: u64,
) -> Box<dyn CredentialStore> {
    if cache_ttl_secs == 0 {
        Box::new(inner)
    } else {
        Box::new(CachedStore::new(
            inner,
            std::time::Duration::from_secs(cache_ttl_secs),
        ))
    }
}

/// Standard credential key for a provider's email (used by Jira).
pub fn email_key(provider: &str) -> String {
    format!("{}/email", provider)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secret(s: &str) -> SecretString {
        SecretString::from(s.to_string())
    }

    fn exposed(s: &Option<SecretString>) -> Option<&str> {
        s.as_ref().map(|v| v.expose_secret())
    }

    #[test]
    fn test_memory_store_basic() {
        let store = MemoryStore::new();

        // Store
        store.store("test/key", &secret("test-value")).unwrap();

        // Get
        let value = store.get("test/key").unwrap();
        assert_eq!(exposed(&value), Some("test-value"));

        // Exists
        assert!(store.exists("test/key"));
        assert!(!store.exists("nonexistent"));

        // Delete
        store.delete("test/key").unwrap();
        let value = store.get("test/key").unwrap();
        assert!(value.is_none());

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
            exposed(&store.get("gitlab/token").unwrap()),
            Some("glpat-xxx")
        );
        assert_eq!(
            exposed(&store.get("github/token").unwrap()),
            Some("ghp-yyy")
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
        assert!(store.get("nonexistent/key").unwrap().is_none());
    }

    #[test]
    fn test_memory_store_exists() {
        let store = MemoryStore::new();

        assert!(!store.exists("test/key"));

        store.store("test/key", &secret("value")).unwrap();
        assert!(store.exists("test/key"));

        store.delete("test/key").unwrap();
        assert!(!store.exists("test/key"));
    }

    #[test]
    fn test_memory_store_overwrite() {
        let store = MemoryStore::new();

        store.store("test/key", &secret("value1")).unwrap();
        assert_eq!(exposed(&store.get("test/key").unwrap()), Some("value1"));

        store.store("test/key", &secret("value2")).unwrap();
        assert_eq!(exposed(&store.get("test/key").unwrap()), Some("value2"));
    }

    #[test]
    fn test_credential_store_exists_default_impl() {
        // Test the default exists() impl from the trait
        let store = MemoryStore::new();

        store.store("key1", &secret("val1")).unwrap();

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

    /// Mock env reader that returns values from a static map.
    fn mock_env_reader(key: &str) -> std::result::Result<String, std::env::VarError> {
        match key {
            "DEVBOY_TEST_TOKEN" => Ok("prefixed-value".into()),
            "TEST_FALLBACK_TOKEN" => Ok("unprefixed-value".into()),
            "DEVBOY_TEST_PRIORITY_TOKEN" => Ok("prefixed".into()),
            "TEST_PRIORITY_TOKEN" => Ok("unprefixed".into()),
            "TEST_NO_FALLBACK_TOKEN" => Ok("unprefixed-value".into()),
            "DEVBOY_CHAIN_TEST_TOKEN" => Ok("from-env".into()),
            _ => Err(std::env::VarError::NotPresent),
        }
    }

    #[test]
    fn test_env_var_store_get_prefixed() {
        let store = EnvVarStore::new().with_env_reader(mock_env_reader);

        let result = store.get("test.token").unwrap();
        assert_eq!(exposed(&result), Some("prefixed-value"));
    }

    #[test]
    fn test_env_var_store_get_unprefixed_fallback() {
        let store = EnvVarStore::new().with_env_reader(mock_env_reader);

        let result = store.get("test.fallback.token").unwrap();
        assert_eq!(exposed(&result), Some("unprefixed-value"));
    }

    #[test]
    fn test_env_var_store_prefixed_takes_priority() {
        let store = EnvVarStore::new().with_env_reader(mock_env_reader);

        let result = store.get("test.priority.token").unwrap();
        assert_eq!(exposed(&result), Some("prefixed"));
    }

    #[test]
    fn test_env_var_store_no_fallback() {
        let store = EnvVarStore::new()
            .without_fallback()
            .with_env_reader(mock_env_reader);

        // Should NOT find it because fallback is disabled
        // (TEST_NO_FALLBACK_TOKEN exists but only as unprefixed)
        let result = store.get("test.no.fallback.token").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_env_var_store_not_found() {
        let store = EnvVarStore::new().with_env_reader(mock_env_reader);

        let result = store.get("nonexistent.key.that.does.not.exist").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_env_var_store_is_read_only() {
        let store = EnvVarStore::new();

        assert!(!store.is_writable());

        let store_result = store.store("test.key", &secret("value"));
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
        assert_eq!(exposed(&chain.get("key1").unwrap()), Some("value1"));

        // key2 should come from second store (not in first)
        assert_eq!(exposed(&chain.get("key2").unwrap()), Some("value2"));

        // key3 not found in either
        assert!(chain.get("key3").unwrap().is_none());
    }

    #[test]
    fn test_chain_store_store_to_first_writable() {
        // EnvVarStore (read-only) + MemoryStore (writable)
        let chain = ChainStore::new(vec![
            Box::new(EnvVarStore::new()),
            Box::new(MemoryStore::new()),
        ]);

        // Should store to MemoryStore (first writable)
        chain.store("test.key", &secret("test-value")).unwrap();

        // Should retrieve from chain
        assert_eq!(exposed(&chain.get("test.key").unwrap()), Some("test-value"));
    }

    #[test]
    fn test_chain_store_no_writable_store_error() {
        // Chain with only read-only stores
        let chain = ChainStore::new(vec![Box::new(EnvVarStore::new())]);

        let result = chain.store("test.key", &secret("value"));
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("No writable"));
    }

    #[test]
    fn test_chain_store_delete_from_all_writable() {
        let store1 = MemoryStore::new();
        let store2 = MemoryStore::new();

        // Store in both
        store1.store("key", &secret("val1")).unwrap();
        store2.store("key", &secret("val2")).unwrap();

        let chain = ChainStore::new(vec![Box::new(store1), Box::new(store2)]);

        // Delete should remove from both
        chain.delete("key").unwrap();

        // Neither should have the key now
        assert!(chain.get("key").unwrap().is_none());
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

        // Set up env var store with mock reader
        let env_store = EnvVarStore::new().with_env_reader(mock_env_reader);

        // Set up memory store with different value
        let memory = MemoryStore::with_credentials([(
            "chain.test.token".to_string(),
            "from-memory".to_string(),
        )]);

        // Chain: env -> memory
        let chain = ChainStore::new(vec![Box::new(env_store), Box::new(memory)]);

        // Env var should win
        assert_eq!(
            exposed(&chain.get("chain.test.token").unwrap()),
            Some("from-env")
        );
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
            exposed(&chain.get("fallback.test.token").unwrap()),
            Some("from-memory")
        );
    }

    #[test]
    fn test_chain_store_debug_impl() {
        let chain = ChainStore::default_chain();
        let debug_str = format!("{:?}", chain);
        assert!(debug_str.contains("ChainStore"));
        assert!(debug_str.contains("stores_count"));
    }

    // =========================================================================
    // build_default_store / wrap_with_cache factories (Wire-up #2)
    // =========================================================================

    #[test]
    fn test_build_default_store_zero_ttl_returns_writable_chain() {
        let store = build_default_store(0);
        // Default chain: env vars (read-only) + keychain (writable) → overall writable.
        assert!(store.is_writable());
    }

    #[test]
    fn test_build_default_store_positive_ttl_delegates_writable() {
        let store = build_default_store(60);
        // Cache must not break write-capability delegation.
        assert!(store.is_writable());
    }

    #[test]
    fn test_wrap_with_cache_zero_ttl_is_passthrough() {
        let inner = MemoryStore::with_credentials([("k".to_string(), "v".to_string())]);
        let store = wrap_with_cache(inner, 0);
        assert_eq!(exposed(&store.get("k").unwrap()), Some("v"));
    }

    #[test]
    fn test_wrap_with_cache_populated_ttl_caches_lookups() {
        let inner = MemoryStore::with_credentials([("k".to_string(), "v1".to_string())]);
        let store = wrap_with_cache(inner, 60);

        assert_eq!(exposed(&store.get("k").unwrap()), Some("v1"));

        // Second call returns the same value — cached or not, semantics are identical.
        assert_eq!(exposed(&store.get("k").unwrap()), Some("v1"));
    }
}
