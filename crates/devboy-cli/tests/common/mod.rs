//! Common test utilities and mock providers.
//!
//! This module provides test infrastructure for devboy-tools:
//! - `FixtureProvider`: Loads data from JSON fixtures in tests/fixtures/
//! - `TestMode`: Record (real API) or Replay (fixtures) mode detection
//! - `TestProvider`: Provider wrapper with Record/Replay support
//! - `ApiResult`: Result type with fallback support
//!
//! # Test Mode Detection
//!
//! Tests automatically detect whether to use real API or fixtures:
//! - If `{PROVIDER}_TOKEN` env var is set → Record mode (real API calls)
//! - If env var is missing → Replay mode (use fixtures)
//!
//! This allows:
//! - Main repo CI runs with real API (secrets configured)
//! - Forks/contributors run tests with fixtures (no secrets needed)
//!
//! # Error Handling (ADR-003)
//!
//! - 401/403 → Test fails (bad credentials)
//! - 5xx/Network → Fallback to fixtures
//! - Fixtures missing → Test fails

pub mod api_result;
pub mod test_provider;

pub use test_provider::TestProvider;

use std::env;
use std::path::PathBuf;

use devboy_core::{Issue, MergeRequest, Result};

/// Test execution mode.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TestMode {
    /// Use real API and record responses to fixtures
    Record,
    /// Use saved fixtures (no real API calls)
    Replay,
}

impl TestMode {
    /// Detect test mode based on environment variables.
    ///
    /// Checks for `{PROVIDER}_TOKEN` environment variable.
    /// If present → Record mode, otherwise → Replay mode.
    pub fn detect(provider: &str) -> Self {
        let token_var = format!("{}_TOKEN", provider.to_uppercase());
        if env::var(&token_var).is_ok() {
            TestMode::Record
        } else {
            TestMode::Replay
        }
    }

    /// Check if we're in record mode.
    pub fn is_record(&self) -> bool {
        matches!(self, TestMode::Record)
    }

    /// Check if we're in replay mode.
    pub fn is_replay(&self) -> bool {
        matches!(self, TestMode::Replay)
    }
}

/// Provider that loads data from JSON fixtures.
///
/// Used in Replay mode when real API tokens are not available.
#[derive(Debug)]
pub struct FixtureProvider {
    provider_name: String,
    fixtures_dir: PathBuf,
}

impl FixtureProvider {
    /// Create a fixture provider for the given provider name.
    ///
    /// Looks for fixtures in `tests/fixtures/{provider_name}/`
    pub fn new(provider_name: &str) -> Self {
        let fixtures_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join(provider_name);

        Self {
            provider_name: provider_name.to_string(),
            fixtures_dir,
        }
    }

    /// Load issues from fixtures.
    pub fn load_issues(&self) -> Result<Vec<Issue>> {
        let path = self.fixtures_dir.join("issues.json");
        let content = std::fs::read_to_string(&path).map_err(|e| {
            devboy_core::Error::Config(format!(
                "Failed to load fixture {}: {}",
                path.display(),
                e
            ))
        })?;
        let issues: Vec<Issue> = serde_json::from_str(&content)?;
        Ok(issues)
    }

    /// Load merge requests from fixtures.
    pub fn load_merge_requests(&self) -> Result<Vec<MergeRequest>> {
        // GitHub uses pull_requests.json, GitLab uses merge_requests.json
        let path = if self.provider_name == "github" {
            self.fixtures_dir.join("pull_requests.json")
        } else {
            self.fixtures_dir.join("merge_requests.json")
        };

        let content = std::fs::read_to_string(&path).map_err(|e| {
            devboy_core::Error::Config(format!(
                "Failed to load fixture {}: {}",
                path.display(),
                e
            ))
        })?;
        let mrs: Vec<MergeRequest> = serde_json::from_str(&content)?;
        Ok(mrs)
    }

    /// Save issues to fixtures (used in Record mode).
    pub fn save_issues(&self, issues: &[Issue]) -> Result<()> {
        std::fs::create_dir_all(&self.fixtures_dir).map_err(|e| {
            devboy_core::Error::Config(format!(
                "Failed to create fixtures dir {}: {}",
                self.fixtures_dir.display(),
                e
            ))
        })?;

        let path = self.fixtures_dir.join("issues.json");
        let content = serde_json::to_string_pretty(issues)?;
        std::fs::write(&path, content).map_err(|e| {
            devboy_core::Error::Config(format!("Failed to save fixture {}: {}", path.display(), e))
        })?;
        Ok(())
    }

    /// Save merge requests to fixtures (used in Record mode).
    pub fn save_merge_requests(&self, mrs: &[MergeRequest]) -> Result<()> {
        std::fs::create_dir_all(&self.fixtures_dir).map_err(|e| {
            devboy_core::Error::Config(format!(
                "Failed to create fixtures dir {}: {}",
                self.fixtures_dir.display(),
                e
            ))
        })?;

        let path = if self.provider_name == "github" {
            self.fixtures_dir.join("pull_requests.json")
        } else {
            self.fixtures_dir.join("merge_requests.json")
        };

        let content = serde_json::to_string_pretty(mrs)?;
        std::fs::write(&path, content).map_err(|e| {
            devboy_core::Error::Config(format!("Failed to save fixture {}: {}", path.display(), e))
        })?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mode_detect_without_token() {
        // Clean environment
        env::remove_var("TEST_PROVIDER_TOKEN");
        let mode = TestMode::detect("test_provider");
        assert_eq!(mode, TestMode::Replay);
        assert!(mode.is_replay());
        assert!(!mode.is_record());
    }

    #[test]
    fn test_mode_detect_with_token() {
        env::set_var("TEST_PROVIDER_2_TOKEN", "fake-token");
        let mode = TestMode::detect("test_provider_2");
        assert_eq!(mode, TestMode::Record);
        assert!(mode.is_record());
        assert!(!mode.is_replay());
        env::remove_var("TEST_PROVIDER_2_TOKEN");
    }

    #[test]
    fn test_fixture_provider_load_issues() {
        let provider = FixtureProvider::new("gitlab");
        let issues = provider.load_issues().unwrap();
        assert!(!issues.is_empty());
        assert!(issues[0].key.starts_with("gitlab#"));
    }

    #[test]
    fn test_fixture_provider_load_github_issues() {
        let provider = FixtureProvider::new("github");
        let issues = provider.load_issues().unwrap();
        assert!(!issues.is_empty());
        assert!(issues[0].key.starts_with("gh#"));
    }
}
