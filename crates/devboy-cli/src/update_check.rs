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

const GITHUB_OWNER: &str = "meteora-pro";

const GITHUB_REPO: &str = "devboy-tools";

/// Cache TTL: 24 hours.
const CACHE_TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// HTTP request timeout for version check.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// Environment variable to disable update checks.
const NO_UPDATE_CHECK_ENV: &str = "DEVBOY_NO_UPDATE_CHECK";

/// Cached version check result.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(test, derive(PartialEq))]
pub(crate) struct VersionCache {
    /// Latest version from GitHub (without 'v' prefix).
    pub(crate) latest_version: String,
    /// Unix timestamp when the check was performed.
    pub(crate) checked_at: u64,
}

/// Resolved version information for the current installation.
#[derive(Debug, Clone, Serialize)]
pub struct VersionStatus {
    /// Current CLI version.
    pub current_version: String,
    /// Latest version from cache or GitHub, when available.
    pub latest_version: Option<String>,
    /// Whether an update is available.
    pub update_available: bool,
    /// Human-readable installation method.
    pub install_method: String,
    /// Recommended update command for this installation.
    pub update_command: String,
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
    /// Returns the appropriate update command as a display string.
    pub fn update_command(&self) -> &'static str {
        match self {
            InstallMethod::Npm => "npm update -g @devboy-tools/cli",
            InstallMethod::Pnpm => "pnpm update -g @devboy-tools/cli",
            InstallMethod::Yarn => "yarn global upgrade @devboy-tools/cli",
            InstallMethod::Standalone => "devboy upgrade",
        }
    }

    /// Returns the update command as structured `(program, args)` for execution.
    #[cfg(not(windows))]
    pub fn update_command_parts(&self) -> (&'static str, &'static [&'static str]) {
        match self {
            InstallMethod::Npm => ("npm", &["update", "-g", "@devboy-tools/cli"]),
            InstallMethod::Pnpm => ("pnpm", &["update", "-g", "@devboy-tools/cli"]),
            InstallMethod::Yarn => ("yarn", &["global", "upgrade", "@devboy-tools/cli"]),
            InstallMethod::Standalone => ("devboy", &["upgrade"]),
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
        if let Ok(user_agent) = env::var("npm_config_user_agent")
            && user_agent.starts_with("pnpm/")
        {
            return InstallMethod::Pnpm;
        }
        if let Ok(user_agent) = env::var("npm_config_user_agent")
            && user_agent.starts_with("yarn/")
        {
            return InstallMethod::Yarn;
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

/// Read cached version check result from the given path.
pub(crate) fn read_cache_from(path: &std::path::Path) -> Option<VersionCache> {
    let content = fs::read_to_string(path).ok()?;
    let cache: VersionCache = serde_json::from_str(&content).ok()?;

    // Check if cache is still fresh
    let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();

    if now.saturating_sub(cache.checked_at) < CACHE_TTL.as_secs() {
        Some(cache)
    } else {
        None
    }
}

/// Read cached version check result from the default cache path.
fn read_cache() -> Option<VersionCache> {
    let path = cache_path()?;
    read_cache_from(&path)
}

/// Write version check result to the given path.
pub(crate) fn write_cache_to(path: &std::path::Path, latest_version: &str) {
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
        let _ = fs::write(path, content);
    }
}

/// Write version check result to the default cache path.
fn write_cache(latest_version: &str) {
    let Some(path) = cache_path() else {
        return;
    };
    write_cache_to(&path, latest_version);
}

/// Build a GitHub API request with optional authentication.
///
/// Uses `GITHUB_TOKEN` or `GH_TOKEN` env var for authentication if available,
/// which increases the rate limit from 60 to 5000 requests/hour.
fn github_api_request(client: &reqwest::Client, url: &str) -> reqwest::RequestBuilder {
    let mut req = client.get(url);
    if let Ok(token) = env::var("GITHUB_TOKEN").or_else(|_| env::var("GH_TOKEN"))
        && !token.is_empty()
    {
        req = req.bearer_auth(token);
    }
    req
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

    let response = github_api_request(&client, &url).send().await.ok()?;

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

/// Resolve version status using the cache first, then GitHub as a fallback.
pub async fn resolve_version_status() -> VersionStatus {
    let current_version = env!("CARGO_PKG_VERSION").to_string();
    let install_method = detect_install_method();
    let latest_version = if let Some(cache) =
        read_cache().filter(|c| !is_newer_version(&c.latest_version, &current_version))
    {
        // Use cache only if current version is not newer than cached latest.
        // If user upgraded past the cached version, force a re-fetch.
        Some(cache.latest_version)
    } else {
        let fetched = fetch_latest_version().await;
        if let Some(version) = &fetched {
            write_cache(version);
        }
        fetched
    };

    let update_available = latest_version
        .as_deref()
        .is_some_and(|latest| is_newer_version(&current_version, latest));

    VersionStatus {
        current_version,
        latest_version,
        update_available,
        install_method: install_method.name().to_string(),
        update_command: install_method.update_command().to_string(),
    }
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
/// This should be called early in `main()` for non-upgrade commands.
/// Uses cached results when available. When the cache is stale, performs
/// an async HTTP request which can take up to `REQUEST_TIMEOUT` (5s).
pub async fn check_and_notify() {
    if should_skip_check() {
        return;
    }

    let version_status = resolve_version_status().await;
    let Some(latest_version) = version_status.latest_version.as_deref() else {
        return;
    };

    if version_status.update_available {
        let _ = writeln!(
            io::stderr(),
            "\n\x1b[33m⚠ A new version of devboy is available: {} → {}\x1b[0m\n  \
             Update with: \x1b[1m{}\x1b[0m\n",
            version_status.current_version,
            latest_version,
            version_status.update_command
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

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
    fn test_is_newer_version_major_bump() {
        assert!(is_newer_version("1.9.9", "2.0.0"));
        assert!(!is_newer_version("2.0.0", "1.9.9"));
    }

    #[test]
    fn test_is_newer_version_equal() {
        assert!(!is_newer_version("1.0.0", "1.0.0"));
        assert!(!is_newer_version("0.0.0", "0.0.0"));
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
    #[cfg(not(windows))]
    fn test_install_method_update_command_parts() {
        assert_eq!(
            InstallMethod::Npm.update_command_parts(),
            ("npm", &["update", "-g", "@devboy-tools/cli"][..])
        );
        assert_eq!(
            InstallMethod::Pnpm.update_command_parts(),
            ("pnpm", &["update", "-g", "@devboy-tools/cli"][..])
        );
        assert_eq!(
            InstallMethod::Yarn.update_command_parts(),
            ("yarn", &["global", "upgrade", "@devboy-tools/cli"][..])
        );
        assert_eq!(
            InstallMethod::Standalone.update_command_parts(),
            ("devboy", &["upgrade"][..])
        );
    }

    #[test]
    fn test_install_method_is_managed() {
        assert!(InstallMethod::Npm.is_managed());
        assert!(InstallMethod::Pnpm.is_managed());
        assert!(InstallMethod::Yarn.is_managed());
        assert!(!InstallMethod::Standalone.is_managed());
    }

    #[test]
    fn test_install_method_name() {
        assert_eq!(InstallMethod::Npm.name(), "npm");
        assert_eq!(InstallMethod::Pnpm.name(), "pnpm");
        assert_eq!(InstallMethod::Yarn.name(), "yarn");
        assert_eq!(InstallMethod::Standalone.name(), "standalone");
    }

    #[test]
    fn test_cache_write_and_read() {
        let dir = tempfile::tempdir().unwrap();
        let cache_file = dir.path().join("version-check.json");

        write_cache_to(&cache_file, "1.2.3");

        let cache = read_cache_from(&cache_file);
        assert!(cache.is_some(), "Cache should be readable after write");
        let cache = cache.unwrap();
        assert_eq!(cache.latest_version, "1.2.3");
    }

    #[test]
    fn test_cache_expired() {
        let dir = tempfile::tempdir().unwrap();
        let cache_file = dir.path().join("version-check.json");

        // Write a cache entry with a timestamp from 25 hours ago
        let expired_time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            - (25 * 60 * 60);

        let cache = VersionCache {
            latest_version: "1.2.3".to_string(),
            checked_at: expired_time,
        };
        let content = serde_json::to_string(&cache).unwrap();
        fs::write(&cache_file, content).unwrap();

        let result = read_cache_from(&cache_file);
        assert!(result.is_none(), "Expired cache should return None");
    }

    #[test]
    fn test_cache_fresh() {
        let dir = tempfile::tempdir().unwrap();
        let cache_file = dir.path().join("version-check.json");

        // Write a cache entry from 1 hour ago (within 24h TTL)
        let fresh_time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            - (60 * 60);

        let cache = VersionCache {
            latest_version: "2.0.0".to_string(),
            checked_at: fresh_time,
        };
        let content = serde_json::to_string(&cache).unwrap();
        fs::write(&cache_file, content).unwrap();

        let result = read_cache_from(&cache_file);
        assert!(result.is_some(), "Fresh cache should be returned");
        assert_eq!(result.unwrap().latest_version, "2.0.0");
    }

    #[test]
    fn test_cache_nonexistent_file() {
        let dir = tempfile::tempdir().unwrap();
        let cache_file = dir.path().join("nonexistent.json");

        let result = read_cache_from(&cache_file);
        assert!(
            result.is_none(),
            "Nonexistent cache file should return None"
        );
    }

    #[test]
    fn test_cache_invalid_json() {
        let dir = tempfile::tempdir().unwrap();
        let cache_file = dir.path().join("version-check.json");

        fs::write(&cache_file, "not valid json").unwrap();

        let result = read_cache_from(&cache_file);
        assert!(result.is_none(), "Invalid JSON should return None");
    }

    #[test]
    fn test_cache_creates_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let cache_file = dir
            .path()
            .join("nested")
            .join("deep")
            .join("version-check.json");

        write_cache_to(&cache_file, "3.0.0");

        assert!(
            cache_file.exists(),
            "Cache file should be created with parent dirs"
        );
        let cache = read_cache_from(&cache_file);
        assert!(cache.is_some());
        assert_eq!(cache.unwrap().latest_version, "3.0.0");
    }

    #[test]
    fn test_version_cache_serialization_roundtrip() {
        let cache = VersionCache {
            latest_version: "1.2.3".to_string(),
            checked_at: 1700000000,
        };

        let json = serde_json::to_string(&cache).unwrap();
        let deserialized: VersionCache = serde_json::from_str(&json).unwrap();

        assert_eq!(cache, deserialized);
    }

    #[test]
    fn test_cache_path_is_some() {
        // On any platform with a home directory, cache_path should return Some
        let path = cache_path();
        assert!(
            path.is_some(),
            "cache_path() should return Some on this platform"
        );
        let path = path.unwrap();
        assert!(
            path.to_string_lossy().contains("devboy-tools"),
            "Cache path should contain 'devboy-tools': {:?}",
            path
        );
    }
}
