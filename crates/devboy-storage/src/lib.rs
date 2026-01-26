//! Secure credential storage using OS keychain.
//!
//! This crate provides secure storage for API tokens and credentials
//! using the operating system's native keychain/credential manager.

use devboy_core::{Error, Result};

/// Storage service name used in keychain.
const SERVICE_NAME: &str = "devboy-tools";

/// Credential storage trait.
pub trait CredentialStore {
    /// Store a credential securely.
    fn store(&self, key: &str, value: &str) -> Result<()>;

    /// Retrieve a stored credential.
    fn get(&self, key: &str) -> Result<Option<String>>;

    /// Delete a stored credential.
    fn delete(&self, key: &str) -> Result<()>;
}

/// In-memory credential store for testing.
#[derive(Default)]
pub struct MemoryStore {
    credentials: std::sync::RwLock<std::collections::HashMap<String, String>>,
}

impl MemoryStore {
    /// Create a new in-memory store.
    pub fn new() -> Self {
        Self::default()
    }
}

impl CredentialStore for MemoryStore {
    fn store(&self, key: &str, value: &str) -> Result<()> {
        let mut creds = self
            .credentials
            .write()
            .map_err(|e| Error::Storage(e.to_string()))?;
        creds.insert(key.to_string(), value.to_string());
        Ok(())
    }

    fn get(&self, key: &str) -> Result<Option<String>> {
        let creds = self
            .credentials
            .read()
            .map_err(|e| Error::Storage(e.to_string()))?;
        Ok(creds.get(key).cloned())
    }

    fn delete(&self, key: &str) -> Result<()> {
        let mut creds = self
            .credentials
            .write()
            .map_err(|e| Error::Storage(e.to_string()))?;
        creds.remove(key);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_memory_store() {
        let store = MemoryStore::new();

        // Store
        store.store("test-key", "test-value").unwrap();

        // Get
        let value = store.get("test-key").unwrap();
        assert_eq!(value, Some("test-value".to_string()));

        // Delete
        store.delete("test-key").unwrap();
        let value = store.get("test-key").unwrap();
        assert_eq!(value, None);
    }
}
