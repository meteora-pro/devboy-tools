//! Sanitizer errors.

/// Errors returned by the sanitizer.
#[derive(Debug, thiserror::Error)]
pub enum SanitizerError {
    /// A rule definition (TOML) was malformed.
    #[error("invalid rule definition: {0}")]
    InvalidRule(String),

    /// A regex pattern failed to compile.
    #[error("regex compile error: {0}")]
    RegexCompile(#[from] regex::Error),

    /// I/O error reading rule files.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    /// TOML parse error.
    #[error("toml parse error: {0}")]
    TomlParse(#[from] toml::de::Error),
}
