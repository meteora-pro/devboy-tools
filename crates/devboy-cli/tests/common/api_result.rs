//! API result types with fallback support.
//!
//! Implements the Record & Replay pattern from ADR-003.

use std::fmt;

/// Result of an API call with fallback support.
///
/// Used to handle graceful degradation when external APIs are unavailable.
#[derive(Debug)]
pub enum ApiResult<T> {
    /// Successful response from real API
    Ok(T),

    /// Fallback to cached data (API unavailable but test continues)
    Fallback {
        /// Cached data from fixtures
        data: T,
        /// Reason for fallback (e.g., "Server returned 503")
        reason: String,
    },

    /// Configuration error (test should fail)
    ConfigError {
        /// Error message
        message: String,
    },
}

impl<T> ApiResult<T> {
    /// Check if this is a successful result.
    pub fn is_ok(&self) -> bool {
        matches!(self, ApiResult::Ok(_))
    }

    /// Check if this is a fallback result.
    pub fn is_fallback(&self) -> bool {
        matches!(self, ApiResult::Fallback { .. })
    }

    /// Check if this is a config error.
    pub fn is_config_error(&self) -> bool {
        matches!(self, ApiResult::ConfigError { .. })
    }

    /// Get the data, panicking on config error.
    ///
    /// Returns the data for both Ok and Fallback variants.
    /// Panics with the error message for ConfigError.
    pub fn unwrap(self) -> T {
        match self {
            ApiResult::Ok(data) => data,
            ApiResult::Fallback { data, reason } => {
                eprintln!("⚠️  Fallback: {}", reason);
                data
            }
            ApiResult::ConfigError { message } => {
                panic!("❌ Configuration error: {}", message);
            }
        }
    }

    /// Get the data or return an error.
    pub fn into_result(self) -> Result<T, String> {
        match self {
            ApiResult::Ok(data) => Ok(data),
            ApiResult::Fallback { data, reason } => {
                eprintln!("⚠️  Fallback: {}", reason);
                Ok(data)
            }
            ApiResult::ConfigError { message } => Err(message),
        }
    }

    /// Map the inner data.
    pub fn map<U, F: FnOnce(T) -> U>(self, f: F) -> ApiResult<U> {
        match self {
            ApiResult::Ok(data) => ApiResult::Ok(f(data)),
            ApiResult::Fallback { data, reason } => ApiResult::Fallback {
                data: f(data),
                reason,
            },
            ApiResult::ConfigError { message } => ApiResult::ConfigError { message },
        }
    }
}

impl<T: fmt::Debug> fmt::Display for ApiResult<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ApiResult::Ok(data) => write!(f, "Ok({:?})", data),
            ApiResult::Fallback { reason, .. } => write!(f, "Fallback({})", reason),
            ApiResult::ConfigError { message } => write!(f, "ConfigError({})", message),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_api_result_ok() {
        let result: ApiResult<i32> = ApiResult::Ok(42);
        assert!(result.is_ok());
        assert!(!result.is_fallback());
        assert!(!result.is_config_error());
        assert_eq!(result.unwrap(), 42);
    }

    #[test]
    fn test_api_result_fallback() {
        let result: ApiResult<i32> = ApiResult::Fallback {
            data: 42,
            reason: "Server error".to_string(),
        };
        assert!(!result.is_ok());
        assert!(result.is_fallback());
        assert!(!result.is_config_error());
        assert_eq!(result.unwrap(), 42);
    }

    #[test]
    #[should_panic(expected = "Configuration error: Bad token")]
    fn test_api_result_config_error() {
        let result: ApiResult<i32> = ApiResult::ConfigError {
            message: "Bad token".to_string(),
        };
        assert!(!result.is_ok());
        assert!(!result.is_fallback());
        assert!(result.is_config_error());
        result.unwrap(); // Should panic
    }

    #[test]
    fn test_api_result_map() {
        let result: ApiResult<i32> = ApiResult::Ok(21);
        let mapped = result.map(|x| x * 2);
        assert_eq!(mapped.unwrap(), 42);
    }
}
