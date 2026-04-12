//! Configuration for the asset cache subsystem.
//!
//! Mirrors the `[assets]` section of `devboy.toml` and provides human-readable
//! parsers for sizes (e.g. `"1Gi"`) and durations (e.g. `"7d"`).

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;

use crate::error::{AssetError, Result};

/// Default cache size limit: 1 GiB.
pub const DEFAULT_MAX_CACHE_SIZE: &str = "1Gi";

/// Default maximum file age before LRU eviction kicks in: 7 days.
pub const DEFAULT_MAX_FILE_AGE: &str = "7d";

/// Eviction strategy applied by the rotator when the cache is over budget.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum EvictionPolicy {
    /// Least-recently-used (based on `last_accessed_ms` in the index).
    #[default]
    Lru,
    /// First-in-first-out (based on `downloaded_at_ms` in the index).
    Fifo,
    /// Do not evict anything — useful for tests or when an external janitor
    /// is managing cache size.
    None,
}

/// Raw configuration as loaded from `devboy.toml`.
///
/// Use [`AssetConfig::resolve`] to produce a validated [`ResolvedAssetConfig`]
/// with absolute paths and parsed sizes / durations.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct AssetConfig {
    /// Custom cache directory. If unset, falls back to
    /// `dirs::cache_dir()/devboy-tools/assets`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_dir: Option<String>,
    /// Maximum cache size in human-readable form, e.g. `"1Gi"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_cache_size: Option<String>,
    /// Maximum age for an unaccessed file before it becomes eligible for
    /// eviction, e.g. `"7d"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_file_age: Option<String>,
    /// Eviction policy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eviction_policy: Option<EvictionPolicy>,
}

/// Fully validated asset configuration with parsed values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedAssetConfig {
    /// Absolute path to the cache directory.
    pub cache_dir: PathBuf,
    /// Hard limit on total cache size in bytes.
    pub max_cache_size: u64,
    /// Maximum age for an unaccessed file before eviction is allowed.
    pub max_file_age: Duration,
    /// Eviction policy.
    pub eviction_policy: EvictionPolicy,
}

impl AssetConfig {
    /// Parse and validate the configuration, applying defaults where needed.
    pub fn resolve(&self) -> Result<ResolvedAssetConfig> {
        let raw_dir = match self.cache_dir.as_deref() {
            Some(path) => expand_path(path),
            None => default_cache_dir()?,
        };
        // Ensure the resolved path is always absolute. A relative path
        // would make the cache location depend on the process working
        // directory and violate the documented contract on
        // `ResolvedAssetConfig::cache_dir`.
        let cache_dir = if raw_dir.is_relative() {
            std::env::current_dir()
                .map_err(|e| {
                    AssetError::cache_dir(format!("cannot resolve relative cache_dir: {e}"))
                })?
                .join(raw_dir)
        } else {
            raw_dir
        };

        let max_cache_size = parse_size(
            self.max_cache_size
                .as_deref()
                .unwrap_or(DEFAULT_MAX_CACHE_SIZE),
        )?;

        let max_file_age =
            parse_duration(self.max_file_age.as_deref().unwrap_or(DEFAULT_MAX_FILE_AGE))?;

        Ok(ResolvedAssetConfig {
            cache_dir,
            max_cache_size,
            max_file_age,
            eviction_policy: self.eviction_policy.unwrap_or_default(),
        })
    }
}

/// Default cache directory used when the user did not configure one.
pub fn default_cache_dir() -> Result<PathBuf> {
    let base = dirs::cache_dir()
        .ok_or_else(|| AssetError::cache_dir("unable to determine OS cache directory"))?;
    Ok(base.join("devboy-tools").join("assets"))
}

/// Expand a user-provided path — supports leading `~` and `$HOME` only.
///
/// We intentionally avoid pulling in a shell-expansion crate: configuration
/// paths are authored by a human and the two cases above cover everything
/// real users do.
pub fn expand_path(input: &str) -> PathBuf {
    if let Some(rest) = input.strip_prefix("~/")
        && let Some(home) = dirs::home_dir()
    {
        return home.join(rest);
    }
    if input == "~"
        && let Some(home) = dirs::home_dir()
    {
        return home;
    }
    if let Some(rest) = input.strip_prefix("$HOME/")
        && let Some(home) = dirs::home_dir()
    {
        return home.join(rest);
    }
    PathBuf::from(input)
}

// =============================================================================
// Human-readable parsers
// =============================================================================

/// Parse a human-readable size string into a byte count.
///
/// Accepts:
/// - Plain numbers: `1024` → 1024 bytes
/// - SI: `K`, `M`, `G`, `T` (1000-based)
/// - Binary: `Ki`, `Mi`, `Gi`, `Ti` (1024-based)
/// - With an optional trailing `B` (e.g. `1Gi`, `1GiB`)
/// - Case-insensitive; whitespace between digits and unit is ignored
pub fn parse_size(input: &str) -> Result<u64> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(AssetError::config("size value is empty"));
    }

    let (number_part, unit_part) = split_value_and_unit(trimmed);
    let number: f64 = number_part
        .parse()
        .map_err(|_| AssetError::config(format!("invalid size number: {number_part}")))?;

    if !number.is_finite() {
        return Err(AssetError::config(format!(
            "non-finite size number: {input}",
        )));
    }
    if number < 0.0 {
        return Err(AssetError::config(format!("negative size: {input}")));
    }

    // Normalize: strip trailing B/b and lowercase
    let mut unit = unit_part.to_ascii_lowercase();
    if unit.ends_with('b') && unit != "b" {
        unit.pop();
    }
    if unit == "b" {
        unit.clear();
    }

    let multiplier: f64 = match unit.as_str() {
        "" => 1.0,
        "k" => 1_000.0,
        "m" => 1_000_000.0,
        "g" => 1_000_000_000.0,
        "t" => 1_000_000_000_000.0,
        "ki" => 1024.0,
        "mi" => 1024f64.powi(2),
        "gi" => 1024f64.powi(3),
        "ti" => 1024f64.powi(4),
        other => {
            return Err(AssetError::config(format!("unknown size unit: {other}")));
        }
    };

    // Check the result is still in range. `as u64` would otherwise clamp
    // NaN to 0 and silently cap anything past `u64::MAX`.
    let bytes = number * multiplier;
    if !bytes.is_finite() || bytes < 0.0 || bytes > u64::MAX as f64 {
        return Err(AssetError::config(format!(
            "size overflows u64 bytes: {input}",
        )));
    }
    Ok(bytes as u64)
}

/// Parse a human-readable duration such as `"7d"`, `"24h"`, `"30m"`, `"45s"`.
///
/// The suffix is required and may be one of `s`, `m`, `h`, `d`, `w`.
/// Whitespace between the number and the suffix is allowed.
pub fn parse_duration(input: &str) -> Result<Duration> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(AssetError::config("duration value is empty"));
    }

    let (number_part, unit_part) = split_value_and_unit(trimmed);
    if unit_part.is_empty() {
        return Err(AssetError::config(format!(
            "duration requires a unit suffix (s/m/h/d/w): {input}"
        )));
    }

    let number: u64 = number_part
        .parse()
        .map_err(|_| AssetError::config(format!("invalid duration number: {number_part}")))?;

    let multiplier: u64 = match unit_part.to_ascii_lowercase().as_str() {
        "s" => 1,
        "m" => 60,
        "h" => 3_600,
        "d" => 86_400,
        "w" => 604_800,
        other => {
            return Err(AssetError::config(format!(
                "unknown duration unit: {other}"
            )));
        }
    };

    // Use checked_mul so comically large inputs surface as a config error
    // instead of silently wrapping in release builds.
    let seconds = number
        .checked_mul(multiplier)
        .ok_or_else(|| AssetError::config(format!("duration overflows u64 seconds: {input}",)))?;

    Ok(Duration::from_secs(seconds))
}

/// Split a human-readable value string into numeric and unit parts.
fn split_value_and_unit(input: &str) -> (&str, &str) {
    let split_at = input
        .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-'))
        .unwrap_or(input.len());
    let (num, rest) = input.split_at(split_at);
    (num.trim(), rest.trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn size_parses_binary_units() {
        assert_eq!(parse_size("1Ki").unwrap(), 1024);
        assert_eq!(parse_size("1Mi").unwrap(), 1024 * 1024);
        assert_eq!(parse_size("1Gi").unwrap(), 1024 * 1024 * 1024);
        assert_eq!(parse_size("2GiB").unwrap(), 2 * 1024 * 1024 * 1024);
    }

    #[test]
    fn size_parses_si_units() {
        assert_eq!(parse_size("1K").unwrap(), 1_000);
        assert_eq!(parse_size("5M").unwrap(), 5_000_000);
        assert_eq!(parse_size("1G").unwrap(), 1_000_000_000);
    }

    #[test]
    fn size_parses_plain_bytes() {
        assert_eq!(parse_size("1024").unwrap(), 1024);
        assert_eq!(parse_size("100B").unwrap(), 100);
        assert_eq!(parse_size("0").unwrap(), 0);
    }

    #[test]
    fn size_is_case_insensitive_and_whitespace_tolerant() {
        assert_eq!(parse_size("1 gi").unwrap(), 1024 * 1024 * 1024);
        assert_eq!(parse_size("  2  MI ").unwrap(), 2 * 1024 * 1024);
    }

    #[test]
    fn size_rejects_garbage() {
        assert!(parse_size("").is_err());
        assert!(parse_size("abc").is_err());
        assert!(parse_size("1Zi").is_err());
        assert!(parse_size("-1Gi").is_err());
    }

    #[test]
    fn size_rejects_non_finite_values() {
        // NaN / infinity parse as f64 but must be rejected — otherwise
        // `as u64` would silently cast them to 0 or clamp.
        assert!(parse_size("NaN").is_err());
        assert!(parse_size("NaNGi").is_err());
        assert!(parse_size("inf").is_err());
        assert!(parse_size("infGi").is_err());
    }

    #[test]
    fn size_rejects_overflow() {
        // 1e30 bytes is far beyond u64::MAX (~1.8e19).
        assert!(parse_size("1e30").is_err());
    }

    #[test]
    fn duration_parses_common_suffixes() {
        assert_eq!(parse_duration("45s").unwrap(), Duration::from_secs(45));
        assert_eq!(parse_duration("30m").unwrap(), Duration::from_secs(30 * 60));
        assert_eq!(parse_duration("24h").unwrap(), Duration::from_secs(86_400));
        assert_eq!(
            parse_duration("7d").unwrap(),
            Duration::from_secs(7 * 86_400)
        );
        assert_eq!(
            parse_duration("2w").unwrap(),
            Duration::from_secs(2 * 7 * 86_400)
        );
    }

    #[test]
    fn duration_rejects_invalid_input() {
        assert!(parse_duration("").is_err());
        assert!(parse_duration("10").is_err(), "missing unit");
        assert!(parse_duration("abc").is_err());
        assert!(parse_duration("1y").is_err(), "years not supported");
    }

    #[test]
    fn duration_detects_overflow() {
        // u64::MAX weeks is guaranteed to overflow: 604_800 * MAX >> u64::MAX.
        let huge = format!("{}w", u64::MAX);
        let err = parse_duration(&huge).unwrap_err();
        assert!(matches!(err, AssetError::Config(_)));
        assert!(err.to_string().contains("overflows"));
    }

    #[test]
    fn expand_path_handles_tilde() {
        let expanded = expand_path("~/foo");
        let home = dirs::home_dir().unwrap();
        assert_eq!(expanded, home.join("foo"));

        let plain = expand_path("/tmp/devboy");
        assert_eq!(plain, PathBuf::from("/tmp/devboy"));
    }

    #[test]
    fn resolve_uses_defaults_when_empty() {
        let cfg = AssetConfig::default();
        let resolved = cfg.resolve().unwrap();
        assert_eq!(
            resolved.max_cache_size,
            parse_size(DEFAULT_MAX_CACHE_SIZE).unwrap()
        );
        assert_eq!(
            resolved.max_file_age,
            parse_duration(DEFAULT_MAX_FILE_AGE).unwrap()
        );
        assert_eq!(resolved.eviction_policy, EvictionPolicy::Lru);
    }

    #[test]
    fn resolve_honors_user_values() {
        let cfg = AssetConfig {
            cache_dir: Some("/tmp/devboy-test".into()),
            max_cache_size: Some("500Mi".into()),
            max_file_age: Some("24h".into()),
            eviction_policy: Some(EvictionPolicy::Fifo),
        };
        let resolved = cfg.resolve().unwrap();
        assert_eq!(resolved.cache_dir, PathBuf::from("/tmp/devboy-test"));
        assert_eq!(resolved.max_cache_size, 500 * 1024 * 1024);
        assert_eq!(resolved.max_file_age, Duration::from_secs(86_400));
        assert_eq!(resolved.eviction_policy, EvictionPolicy::Fifo);
    }

    #[test]
    fn eviction_policy_serde() {
        let toml_str = r#"eviction_policy = "lru""#;
        let cfg: AssetConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.eviction_policy, Some(EvictionPolicy::Lru));

        let toml_str = r#"eviction_policy = "none""#;
        let cfg: AssetConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.eviction_policy, Some(EvictionPolicy::None));
    }

    #[test]
    fn asset_config_toml_roundtrip() {
        let toml_str = r#"
cache_dir = "/tmp/custom"
max_cache_size = "2Gi"
max_file_age = "3d"
eviction_policy = "lru"
"#;
        let cfg: AssetConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.cache_dir.as_deref(), Some("/tmp/custom"));
        assert_eq!(cfg.max_cache_size.as_deref(), Some("2Gi"));

        let resolved = cfg.resolve().unwrap();
        assert_eq!(resolved.max_cache_size, 2 * 1024 * 1024 * 1024);
        assert_eq!(resolved.max_file_age, Duration::from_secs(3 * 86_400));
    }
}
