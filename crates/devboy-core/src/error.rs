//! Error types for devboy-tools.
//!
//! This module provides a unified error handling system that works across
//! all providers and components.

use thiserror::Error;

/// Main error type for devboy operations.
#[derive(Error, Debug)]
pub enum Error {
    // =========================================================================
    // HTTP / Network Errors
    // =========================================================================
    /// HTTP request failed
    #[error("HTTP error: {0}")]
    Http(String),

    /// Network connectivity error
    #[error("Network error: {0}")]
    Network(String),

    /// Request timeout
    #[error("Request timeout")]
    Timeout,

    // =========================================================================
    // Authentication / Authorization Errors
    // =========================================================================
    /// 401 Unauthorized - invalid or missing credentials
    #[error("Unauthorized: {0}")]
    Unauthorized(String),

    /// 403 Forbidden - valid credentials but insufficient permissions
    #[error("Forbidden: {0}")]
    Forbidden(String),

    // =========================================================================
    // API Errors
    // =========================================================================
    /// API returned an error response
    #[error("API error ({status}): {message}")]
    Api {
        /// HTTP status code
        status: u16,
        /// Error message from API
        message: String,
    },

    /// Resource not found (404)
    #[error("Not found: {0}")]
    NotFound(String),

    /// Rate limit exceeded (429)
    #[error("Rate limit exceeded: retry after {retry_after:?}s")]
    RateLimited {
        /// Seconds to wait before retry
        retry_after: Option<u64>,
    },

    /// Server error (5xx)
    #[error("Server error ({status}): {message}")]
    ServerError {
        /// HTTP status code
        status: u16,
        /// Error message
        message: String,
    },

    // =========================================================================
    // Data Errors
    // =========================================================================
    /// Serialization/deserialization failed
    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    /// Invalid data format or content
    #[error("Invalid data: {0}")]
    InvalidData(String),

    // =========================================================================
    // Configuration Errors
    // =========================================================================
    /// Configuration error
    #[error("Configuration error: {0}")]
    Config(String),

    /// Missing required configuration
    #[error("Missing configuration: {0}")]
    MissingConfig(String),

    // =========================================================================
    // Storage Errors
    // =========================================================================
    /// Storage/keychain error
    #[error("Storage error: {0}")]
    Storage(String),

    /// Credential not found in keychain
    #[error("Credential not found: {provider}/{key}")]
    CredentialNotFound {
        /// Provider name
        provider: String,
        /// Credential key
        key: String,
    },

    // =========================================================================
    // Provider Errors
    // =========================================================================
    /// Provider not found or not configured
    #[error("Provider not found: {0}")]
    ProviderNotFound(String),

    /// Provider not supported for this operation
    #[error("Provider '{provider}' does not support: {operation}")]
    ProviderUnsupported {
        /// Provider name
        provider: String,
        /// Unsupported operation
        operation: String,
    },

    // =========================================================================
    // Generic Errors
    // =========================================================================
    /// IO error
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    /// Generic error wrapper
    #[error("{0}")]
    Other(#[from] anyhow::Error),
}

impl Error {
    /// Create an API error from HTTP status and message.
    pub fn from_status(status: u16, message: impl Into<String>) -> Self {
        let message = message.into();
        match status {
            401 => Error::Unauthorized(message),
            403 => Error::Forbidden(message),
            404 => Error::NotFound(message),
            429 => Error::RateLimited { retry_after: None },
            500..=599 => Error::ServerError { status, message },
            _ => Error::Api { status, message },
        }
    }

    /// Check if this is a retryable error.
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            Error::Timeout
                | Error::Network(_)
                | Error::RateLimited { .. }
                | Error::ServerError { .. }
        )
    }

    /// Check if this is an authentication error.
    pub fn is_auth_error(&self) -> bool {
        matches!(self, Error::Unauthorized(_) | Error::Forbidden(_))
    }
}

/// Result type alias for devboy operations.
pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_from_status() {
        assert!(matches!(
            Error::from_status(401, "test"),
            Error::Unauthorized(_)
        ));
        assert!(matches!(
            Error::from_status(403, "test"),
            Error::Forbidden(_)
        ));
        assert!(matches!(
            Error::from_status(404, "test"),
            Error::NotFound(_)
        ));
        assert!(matches!(
            Error::from_status(429, "test"),
            Error::RateLimited { .. }
        ));
        assert!(matches!(
            Error::from_status(500, "test"),
            Error::ServerError { .. }
        ));
        assert!(matches!(Error::from_status(400, "test"), Error::Api { .. }));
    }

    #[test]
    fn test_is_retryable() {
        assert!(Error::Timeout.is_retryable());
        assert!(Error::Network("test".into()).is_retryable());
        assert!(Error::RateLimited { retry_after: None }.is_retryable());
        assert!(Error::ServerError {
            status: 500,
            message: "test".into()
        }
        .is_retryable());
        assert!(!Error::Unauthorized("test".into()).is_retryable());
        assert!(!Error::NotFound("test".into()).is_retryable());
    }

    #[test]
    fn test_is_auth_error() {
        assert!(Error::Unauthorized("test".into()).is_auth_error());
        assert!(Error::Forbidden("test".into()).is_auth_error());
        assert!(!Error::NotFound("test".into()).is_auth_error());
    }
}
