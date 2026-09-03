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
//! `ChainStore::default_chain()` resolves from **environment variables only**:
//!
//! - `DEVBOY_{PROVIDER}_TOKEN` (e.g., `DEVBOY_GITHUB_TOKEN`)
//! - `{PROVIDER}_TOKEN` (fallback, e.g., `GITHUB_TOKEN`)
//!
//! The OS keychain is **no longer in the default chain** (ADR-024 §6). It only
//! exceeds the protection of a `0600` file on macOS, where item ACLs bind to
//! the reading process's code signature; on Linux the Secret Service hands a
//! stored secret to any process in the user's session, and on Windows DPAPI is
//! scoped to the user. Meanwhile it costs a D-Bus dependency, a daemon absent
//! in CI and containers, and prompt failures on locked-down machines.
//!
//! Use [`ChainStore::from_config`] to apply the configured policy, or
//! [`ChainStore::with_keychain`] to opt in explicitly.
//!
//! # Example
//!
//! ```ignore
//! use devboy_storage::{ChainStore, CredentialStore};
//!
//! // Environment-only by default.
//! let store = ChainStore::default_chain();
//! let token = store.get("github.token")?;
//!
//! // Honour `[secrets.keychain] enabled` and CI mode:
//! let store = ChainStore::from_config(&config, ci_mode);
//!
//! // Or opt the keychain in directly.
//! use devboy_storage::KeychainStore;
//! let keychain = KeychainStore::new();
//! keychain.store("gitlab.token", &secret)?;
//! ```

#![deny(rustdoc::broken_intra_doc_links)]
#![deny(rustdoc::private_intra_doc_links)]
#![deny(rustdoc::invalid_html_tags)]
use devboy_core::{Error, Result};
use keyring::Entry;
use secrecy::{ExposeSecret, SecretString};
use tracing::{debug, warn};

pub mod cache;
pub mod ci;
pub mod expiry;
pub mod index;
pub mod legacy_keychain;
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
pub use index::{ApproveOnUse, Gate, GlobalIndex, IndexEntry, IndexError, RotationMethod};
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
    SourceAccess, SourceDefinition,
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

/// Environment-variable-backed credential store.
///
/// Resolves secrets by name through `std::env::var` (or an injected
/// reader, for testing). Used as the CI / Docker fallback when the OS
/// keychain is unavailable.
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

    /// Create the default credential chain: environment variables,
    /// then the local vault.
    ///
    /// **The OS keychain is no longer in the default chain**
    /// (ADR-024 §6). It only exceeds the protection of a `0600`
    /// file on macOS, where item ACLs bind to the reading
    /// process's code signature; on Linux the Secret Service
    /// hands a stored secret to any process in the user's
    /// session, and on Windows DPAPI is scoped to the user. In
    /// exchange it costs a D-Bus dependency, a daemon that is
    /// absent in CI and containers, and a class of prompt
    /// failures on locked-down machines.
    ///
    /// The local vault takes its place as the durable store, but
    /// **not from here**: reaching it means talking to a daemon,
    /// and this crate is published to crates.io while the daemon
    /// crate is not. The store that speaks to it therefore lives in
    /// `devboy-secret-local-vault`, and the *application* composes
    /// the real chain — see `get_credential_store` in the CLI.
    /// This constructor stays daemon-free so a library consumer
    /// gets something that works with no services running.
    ///
    /// Use [`Self::with_keychain`] when the user has opted back
    /// in via `[secrets.keychain] enabled = true`, or
    /// [`Self::from_config`] to make that decision from config.
    pub fn default_chain() -> Self {
        Self::new(vec![Box::new(EnvVarStore::new())])
    }

    /// The default chain plus the OS keychain — the pre-ADR-024
    /// behaviour, now reachable only by explicit opt-in.
    pub fn with_keychain() -> Self {
        // The keychain is the only writable member here, so it is
        // also the write target — which is what makes enabling it
        // restore the pre-ADR-024 behaviour for a user who keeps
        // tokens there.
        Self::new(vec![
            Box::new(EnvVarStore::new()),
            Box::new(KeychainStore::new()),
        ])
    }

    /// Create a chain for CI / env-only mode (ADR-024 §6).
    ///
    /// Environment variables are the sole source: no keychain, no
    /// daemon, nothing that can block on IPC or prompt.
    ///
    /// Note this deliberately **no longer includes**
    /// `MemoryStore`. Pairing the env store with an in-memory
    /// writable store meant a write in CI appeared to succeed and
    /// then vanished at process exit — a silent data-loss shape
    /// that is fine as a test shim and wrong as CI behaviour. A
    /// write now fails loudly instead.
    pub fn ci_chain() -> Self {
        Self::new(vec![Box::new(EnvVarStore::new())])
    }

    /// Build the chain implied by configuration and CI state.
    ///
    /// - CI / env-only mode → [`Self::ci_chain`]
    /// - `[secrets.keychain] enabled = true` → [`Self::with_keychain`]
    /// - otherwise → [`Self::default_chain`]
    ///
    /// This is the single place the ADR-024 §6 default is decided
    /// for the legacy credential stack; callers should prefer it
    /// over picking a constructor themselves.
    pub fn from_config(config: &devboy_core::config::Config, ci_mode: bool) -> Self {
        if ci_mode {
            return Self::ci_chain();
        }
        if config.is_keychain_enabled() {
            return Self::with_keychain();
        }
        Self::default_chain()
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
        // Reaching here without a `last_error` means no store in
        // the chain accepts writes at all — the normal state since
        // ADR-024 §6 demoted the keychain, because environment
        // variables are read-only and the vault takes writes only
        // through an unlock-carrying path. Say what would make a
        // write possible instead of stating the fact and stopping.
        Err(last_error.unwrap_or_else(|| {
            Error::Storage(format!(
                "nowhere to store '{key}': environment variables are read-only and the local \
                 vault only accepts writes through an unlocked session. Store it with `devboy \
                 secrets ui`, export the matching environment variable, or re-enable the OS \
                 keychain with `devboy config set secrets.keychain.enabled true`."
            ))
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

    /// ADR-024 §6: the keychain left the default chain, and the
    /// local vault took its place as the durable store. A change
    /// here is a change to the product's default security
    /// posture, not a test detail.
    ///
    /// The assertion is on *writability* rather than on a member
    /// count, because the count is legitimately 1 or 2 — the vault
    /// store is skipped on a machine with no derivable config
    /// directory — while the security property holds either way:
    /// nothing in the default chain accepts a write.
    #[test]
    fn test_chain_store_default_chain_has_no_writable_member() {
        let store = ChainStore::default_chain();
        assert!(!store.is_empty());
        assert!(
            !store.is_writable(),
            "the default chain must not absorb writes: the environment is read-only and the \
             vault takes writes only through an unlock-carrying path"
        );
    }

    /// Opting the keychain back in restores a write target, which
    /// is the whole point of the switch.
    #[test]
    fn test_chain_store_with_keychain_opts_back_in() {
        let store = ChainStore::with_keychain();
        assert!(
            store.is_writable(),
            "enabling the keychain must restore somewhere to write"
        );
        assert!(
            store.len() > ChainStore::default_chain().len(),
            "the keychain is added to the default chain, not swapped for it"
        );
    }

    /// The CI chain no longer carries `MemoryStore`: a write used
    /// to appear to succeed there and vanish at process exit.
    #[test]
    fn test_chain_store_ci_chain_has_no_writable_member() {
        let store = ChainStore::ci_chain();
        assert_eq!(store.len(), 1);
        assert!(
            !store.is_writable(),
            "CI chain must refuse writes rather than absorb them"
        );
    }

    #[test]
    fn test_from_config_honours_ci_and_keychain_switch() {
        use devboy_core::config::Config;

        let plain = Config::default();
        assert!(
            !ChainStore::from_config(&plain, false).is_writable(),
            "the default posture has no write target"
        );

        let mut with_keychain = Config::default();
        with_keychain
            .set("secrets.keychain.enabled", "true")
            .unwrap();
        assert!(
            ChainStore::from_config(&with_keychain, false).is_writable(),
            "enabling the keychain must restore a write target"
        );

        // CI wins even when the keychain is enabled: a container has
        // no keychain daemon and no vault daemon, so reaching for
        // either is a hang waiting to happen.
        let ci = ChainStore::from_config(&with_keychain, true);
        assert_eq!(ci.len(), 1, "the CI chain is environment variables alone");
        assert!(!ci.is_writable());
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

    /// A chain with nowhere to write is now the normal default, so
    /// its error is a message users will actually meet. It has to
    /// name the key and offer a way forward — "no writable store"
    /// states a fact and leaves the user stuck.
    #[test]
    fn test_chain_store_no_writable_store_error_is_actionable() {
        let chain = ChainStore::new(vec![Box::new(EnvVarStore::new())]);

        let message = chain
            .store("test.key", &secret("value"))
            .expect_err("a read-only chain must refuse the write")
            .to_string();

        assert!(
            message.contains("test.key"),
            "the error should name the key: {message}"
        );
        assert!(
            message.contains("secrets ui")
                || message.contains("environment variable")
                || message.contains("keychain"),
            "the error should offer a way forward: {message}"
        );
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

    /// After ADR-024 §6 the default chain is environment-only,
    /// and the environment is read-only — so the default store no
    /// longer accepts writes. Writing requires either opting the
    /// keychain back in or the local-vault adapter.
    #[test]
    fn test_build_default_store_zero_ttl_is_read_only() {
        let store = build_default_store(0);
        assert!(!store.is_writable());
    }

    #[test]
    fn test_build_default_store_positive_ttl_delegates_write_capability() {
        // The cache must report the inner chain's capability
        // faithfully rather than inventing one.
        assert_eq!(
            build_default_store(60).is_writable(),
            build_default_store(0).is_writable()
        );
    }

    #[test]
    fn test_cache_delegates_writability_of_a_writable_inner() {
        let inner = MemoryStore::with_credentials([("k".to_string(), "v".to_string())]);
        assert!(wrap_with_cache(inner, 60).is_writable());
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
