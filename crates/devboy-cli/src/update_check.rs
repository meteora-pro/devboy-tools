//! Version update check with caching.
//!
//! Checks GitHub Releases for newer versions and notifies the user via stderr.
//! Results are cached for 24 hours to avoid excessive API calls.

use std::env;
use std::fs;
use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// GitHub repository owner.
const GITHUB_OWNER: &str = "meteora-pro";

/// GitHub repository name.
const GITHUB_REPO: &str = "devboy-tools";

/// Cache TTL: 24 hours.
const CACHE_TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// HTTP request timeout for version check.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// Environment variable to disable update checks.
const NO_UPDATE_CHECK_ENV: &str = "DEVBOY_NO_UPDATE_CHECK";

/// Cached version check result.
#[derive(Debug, Serialize, Deserialize)]
struct VersionCache {
    /// Latest version from GitHub (without 'v' prefix).
    latest_version: String,
    /// Unix timestamp when the check was performed.
    checked_at: u64,
}

/// Detected installation method.
#[derive(Debug, PartialEq)]
pub enum InstallMethod {
    /// Installed via npm (`npm install -g`).
    Npm,
    /// Installed via pnpm (`pnpm add -g`).
    Pnpm,
    /// Installed via yarn (`yarn global add`).
    Yarn,
    /// Standalone binary (GitHub release, cargo install, etc.).
    Standalone,
}

impl InstallMethod {
    /// Returns the appropriate update command for this installation method.
    pub fn update_command(&self) -> &'static str {
        match self {
            InstallMethod::Npm => "npm update -g @devboy-tools/cli",
            InstallMethod::Pnpm => "pnpm update -g @devboy-tools/cli",
            InstallMethod::Yarn => "yarn global upgrade @devboy-tools/cli",
            InstallMethod::Standalone => "devboy upgrade",
        }
    }

    /// Returns true if this is a package-manager-managed installation.
    pub fn is_managed(&self) -> bool {
        !matches!(self, InstallMethod::Standalone)
    }

    /// Returns a human-readable name.
    pub fn name(&self) -> &'static str {
        match self {
            InstallMethod::Npm => "npm",
            InstallMethod::Pnpm => "pnpm",
            InstallMethod::Yarn => "yarn",
            InstallMethod::Standalone => "standalone",
        }
    }
}

/// Detect how devboy was installed by examining the binary path
/// and environment variables.
pub fn detect_install_method() -> InstallMethod {
    // Check if binary is inside node_modules (npm/pnpm/yarn)
    let is_node_modules = env::current_exe()
        .ok()
        .and_then(|p| p.canonicalize().ok())
        .map(|p| p.to_string_lossy().contains("node_modules"))
        .unwrap_or(false);

    if is_node_modules {
        // Try to detect specific package manager from npm_config_user_agent
        // Format: "npm/10.x.x node/22.x.x ..." or "pnpm/9.x.x ..." or "yarn/4.x.x ..."
        if let Ok(user_agent) = env::var("npm_config_user_agent") {
            if user_agent.starts_with("pnpm/") {
                return InstallMethod::Pnpm;
            }
            if user_agent.starts_with("yarn/") {
                return InstallMethod::Yarn;
            }
        }

        // Check pnpm global store path pattern as fallback
        if let Ok(exe) = env::current_exe() {
            let path_str = exe.to_string_lossy();
            if path_str.contains("pnpm") {
                return InstallMethod::Pnpm;
            }
            if path_str.contains("yarn") {
                return InstallMethod::Yarn;
            }
        }

        return InstallMethod::Npm;
    }

    InstallMethod::Standalone
}

/// Check if update check should be skipped.
fn should_skip_check() -> bool {
    // Skip in CI environments
    if env::var("CI").is_ok() {
        return true;
    }

    // Skip if user explicitly disabled
    if env::var(NO_UPDATE_CHECK_ENV)
        .map(|v| v == "1" || v.to_lowercase() == "true")
        .unwrap_or(false)
    {
        return true;
    }

    // Skip if stderr is not a TTY
    if !io::stderr().is_terminal() {
        return true;
    }

    false
}

/// Get the cache file path.
fn cache_path() -> Option<PathBuf> {
    dirs::cache_dir().map(|d| d.join("devboy-tools").join("version-check.json"))
}

/// Read cached version check result.
fn read_cache() -> Option<VersionCache> {
    let path = cache_path()?;
    let content = fs::read_to_string(&path).ok()?;
    let cache: VersionCache = serde_json::from_str(&content).ok()?;

    // Check if cache is still fresh
    let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();

    if now.saturating_sub(cache.checked_at) < CACHE_TTL.as_secs() {
        Some(cache)
    } else {
        None
    }
}

/// Write version check result to cache.
fn write_cache(latest_version: &str) {
    let Some(path) = cache_path() else {
        return;
    };

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let cache = VersionCache {
        latest_version: latest_version.to_string(),
        checked_at: now,
    };

    if let Ok(content) = serde_json::to_string(&cache) {
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let _ = fs::write(&path, content);
    }
}

/// Fetch the latest version from GitHub Releases API.
async fn fetch_latest_version() -> Option<String> {
    let url = format!(
        "https://api.github.com/repos/{}/{}/releases/latest",
        GITHUB_OWNER, GITHUB_REPO
    );

    let client = reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .user_agent(format!("devboy/{}", env!("CARGO_PKG_VERSION")))
        .build()
        .ok()?;

    let response = client.get(&url).send().await.ok()?;

    if !response.status().is_success() {
        return None;
    }

    #[derive(Deserialize)]
    struct Release {
        tag_name: String,
    }

    let release: Release = response.json().await.ok()?;

    // Strip 'v' prefix: "v0.9.0" -> "0.9.0"
    let version = release
        .tag_name
        .strip_prefix('v')
        .unwrap_or(&release.tag_name);
    Some(version.to_string())
}

/// Compare two semver-like version strings.
/// Returns true if `latest` is newer than `current`.
pub fn is_newer_version(current: &str, latest: &str) -> bool {
    let parse = |v: &str| -> Option<(u64, u64, u64)> {
        // Strip any pre-release suffix for comparison
        let v = v.split('-').next().unwrap_or(v);
        let parts: Vec<&str> = v.split('.').collect();
        if parts.len() != 3 {
            return None;
        }
        Some((
            parts[0].parse().ok()?,
            parts[1].parse().ok()?,
            parts[2].parse().ok()?,
        ))
    };

    match (parse(current), parse(latest)) {
        (Some(c), Some(l)) => l > c,
        _ => false,
    }
}

/// Run the update check and print a notice if a newer version is available.
///
/// This should be called early in main() for non-upgrade commands.
/// It's designed to be non-blocking — uses cached results when available,
/// and performs an async HTTP request only when the cache is stale.
pub async fn check_and_notify() {
    if should_skip_check() {
        return;
    }

    let current_version = env!("CARGO_PKG_VERSION");

    // Try cache first
    let latest_version = if let Some(cache) = read_cache() {
        cache.latest_version
    } else {
        // Fetch from GitHub
        let Some(version) = fetch_latest_version().await else {
            return;
        };
        write_cache(&version);
        version
    };

    if is_newer_version(current_version, &latest_version) {
        let install_method = detect_install_method();
        let update_cmd = install_method.update_command();

        let _ = writeln!(
            io::stderr(),
            "\n\x1b[33m⚠ A new version of devboy is available: {} → {}\x1b[0m\n  \
             Update with: \x1b[1m{}\x1b[0m\n",
            current_version,
            latest_version,
            update_cmd
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_newer_version() {
        assert!(is_newer_version("0.9.0", "0.10.0"));
        assert!(is_newer_version("0.9.0", "1.0.0"));
        assert!(is_newer_version("0.9.0", "0.9.1"));
        assert!(!is_newer_version("0.9.0", "0.9.0"));
        assert!(!is_newer_version("0.10.0", "0.9.0"));
        assert!(!is_newer_version("1.0.0", "0.9.0"));
    }

    #[test]
    fn test_is_newer_version_with_prerelease() {
        assert!(is_newer_version("0.9.0-alpha", "0.10.0"));
        assert!(is_newer_version("0.9.0", "0.10.0-beta"));
    }

    #[test]
    fn test_is_newer_version_invalid() {
        assert!(!is_newer_version("invalid", "0.9.0"));
        assert!(!is_newer_version("0.9.0", "invalid"));
        assert!(!is_newer_version("0.9", "0.10.0"));
    }

    #[test]
    fn test_detect_install_method_standalone() {
        // In test environment, binary is not in node_modules
        assert_eq!(detect_install_method(), InstallMethod::Standalone);
    }

    #[test]
    fn test_install_method_update_command() {
        assert_eq!(
            InstallMethod::Npm.update_command(),
            "npm update -g @devboy-tools/cli"
        );
        assert_eq!(
            InstallMethod::Pnpm.update_command(),
            "pnpm update -g @devboy-tools/cli"
        );
        assert_eq!(
            InstallMethod::Yarn.update_command(),
            "yarn global upgrade @devboy-tools/cli"
        );
        assert_eq!(InstallMethod::Standalone.update_command(), "devboy upgrade");
    }

    #[test]
    fn test_install_method_is_managed() {
        assert!(InstallMethod::Npm.is_managed());
        assert!(InstallMethod::Pnpm.is_managed());
        assert!(InstallMethod::Yarn.is_managed());
        assert!(!InstallMethod::Standalone.is_managed());
    }
}
