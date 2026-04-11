//! Error type for the asset management crate.
//!
//! Wraps [`devboy_core::Error`] with a few asset-specific variants. The
//! [`From`] impls let call sites use `?` with IO, serde_json, and the core
//! error type interchangeably.

use thiserror::Error;

/// Errors that can occur during asset cache operations.
#[derive(Error, Debug)]
pub enum AssetError {
    /// Underlying filesystem I/O failure.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Failed to (de)serialize JSON while reading / writing the index.
    #[error("Index serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    /// Failed to parse a human-readable size / duration in configuration.
    #[error("Configuration error: {0}")]
    Config(String),

    /// The requested asset was not present in the cache index.
    #[error("Asset not found: {0}")]
    NotFound(String),

    /// The cache directory was unreachable or could not be created.
    #[error("Cache directory error: {0}")]
    CacheDir(String),

    /// Anything surfaced from [`devboy_core::Error`].
    #[error(transparent)]
    Core(#[from] devboy_core::Error),
}

/// Convenience alias for `Result<T, AssetError>`.
pub type Result<T> = std::result::Result<T, AssetError>;

impl AssetError {
    /// Create a new configuration error.
    pub fn config(msg: impl Into<String>) -> Self {
        AssetError::Config(msg.into())
    }

    /// Create a new cache directory error.
    pub fn cache_dir(msg: impl Into<String>) -> Self {
        AssetError::CacheDir(msg.into())
    }

    /// Create a new not-found error.
    pub fn not_found(id: impl Into<String>) -> Self {
        AssetError::NotFound(id.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constructor_helpers() {
        let e = AssetError::config("bad value");
        assert!(matches!(e, AssetError::Config(_)));
        assert!(format!("{e}").contains("bad value"));

        let e = AssetError::cache_dir("no perms");
        assert!(matches!(e, AssetError::CacheDir(_)));

        let e = AssetError::not_found("asset-1");
        assert!(matches!(e, AssetError::NotFound(_)));
    }

    #[test]
    fn io_error_is_convertible() {
        let io = std::io::Error::new(std::io::ErrorKind::NotFound, "x");
        let e: AssetError = io.into();
        assert!(matches!(e, AssetError::Io(_)));
    }
}
