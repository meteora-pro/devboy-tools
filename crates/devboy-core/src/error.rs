//! Error types for devboy-tools.

use thiserror::Error;

/// Main error type for devboy operations.
#[derive(Error, Debug)]
pub enum Error {
    /// HTTP request failed
    #[error("HTTP error: {0}")]
    Http(String),

    /// Authentication failed
    #[error("Authentication error: {0}")]
    Auth(String),

    /// API returned an error
    #[error("API error: {status} - {message}")]
    Api { status: u16, message: String },

    /// Serialization/deserialization failed
    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    /// Configuration error
    #[error("Configuration error: {0}")]
    Config(String),

    /// Storage error
    #[error("Storage error: {0}")]
    Storage(String),

    /// Provider not found
    #[error("Provider not found: {0}")]
    ProviderNotFound(String),

    /// Generic error
    #[error("{0}")]
    Other(#[from] anyhow::Error),
}

/// Result type alias for devboy operations.
pub type Result<T> = std::result::Result<T, Error>;
