//! Integration tests for local `.devboy.toml` configuration loading.
//!
//! These tests verify that commands correctly use local `.devboy.toml` when present,
//! prioritizing it over the global config.
//!
//! # Running Tests
//!
//! ```bash
//! cargo test --test local_config_test
//! ```
//!
//! # Related Issue
//!
//! https://github.com/meteora-pro/devboy-tools/issues/39

use std::fs;
use std::process::Command;
use tempfile::TempDir;

/// Get path to the devboy binary.
fn devboy_bin() -> std::path::PathBuf {
    let mut path = std::env::current_exe().unwrap();
    path.pop(); // Remove test binary name
    path.pop(); // Remove deps

    let bin_name = format!("devboy{}", std::env::consts::EXE_SUFFIX);
    path.push(bin_name);
    path
}

/// Create a local `.devboy.toml` with GitHub configuration.
fn create_local_config(temp_dir: &TempDir, owner: &str, repo: &str) {
    let config_content = format!(
        r#"[github]
owner = "{}"
repo = "{}"
"#,
        owner, repo
    );

    fs::write(temp_dir.path().join(".devboy.toml"), config_content).unwrap();
}

// =============================================================================
// Tests for `issues` command using local config
// =============================================================================

/// Test that `devboy issues` loads configuration from local `.devboy.toml`.
///
/// This is the key test for issue #39 - verifying that local config is used.
#[test]
fn test_issues_uses_local_config() {
    let temp_dir = TempDir::new().unwrap();

    // Create local config with specific owner/repo
    create_local_config(&temp_dir, "local-owner", "local-repo");

    let output = Command::new(devboy_bin())
        .args(["issues"])
        .env("DEVBOY_SKIP_KEYCHAIN", "1")
        .current_dir(temp_dir.path())
        .output()
        .expect("Failed to execute command");

    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);

    // The command will fail because there's no token OR because the repo doesn't exist.
    // What matters is that it tried to use the LOCAL config (not global).
    // With load_runtime_config(), it should use local-owner/local-repo from .devboy.toml

    // Check that config was loaded and GitHub API was attempted
    // Either: token missing error, OR API 404 error (proving config was loaded and API was called)
    let config_was_loaded = stderr.contains("GitHub token not set")
        || stderr.contains("Failed to get token")
        || stderr.contains("Failed to fetch issues")
        || stderr.contains("404");

    assert!(
        config_was_loaded,
        "Should load local config and attempt GitHub API. stdout: {}, stderr: {}",
        stdout, stderr
    );

    // Verify it's NOT saying "No provider configured" (which would mean config wasn't loaded)
    assert!(
        !stdout.contains("No provider configured"),
        "Should have found GitHub config from local .devboy.toml"
    );
}

// =============================================================================
// Tests for `mrs` command using local config
// =============================================================================

/// Test that `devboy mrs` loads configuration from local `.devboy.toml`.
#[test]
fn test_mrs_uses_local_config() {
    let temp_dir = TempDir::new().unwrap();

    create_local_config(&temp_dir, "local-owner-mr", "local-repo-mr");

    let output = Command::new(devboy_bin())
        .args(["mrs"])
        .env("DEVBOY_SKIP_KEYCHAIN", "1")
        .current_dir(temp_dir.path())
        .output()
        .expect("Failed to execute command");

    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);

    // Should find GitHub config from local .devboy.toml
    let config_was_loaded = stderr.contains("GitHub token not set")
        || stderr.contains("Failed to get token")
        || stderr.contains("Failed to fetch PRs")
        || stderr.contains("404");

    assert!(
        config_was_loaded,
        "Should load local config and attempt GitHub API. stdout: {}, stderr: {}",
        stdout, stderr
    );

    // Verify it's NOT saying "No provider configured"
    assert!(
        !stdout.contains("No provider configured"),
        "Should have found GitHub config from local .devboy.toml"
    );
}

// =============================================================================
// Tests for `test` command using local config
// =============================================================================

/// Test that `devboy test github` loads configuration from local `.devboy.toml`.
#[test]
fn test_test_command_uses_local_config() {
    let temp_dir = TempDir::new().unwrap();

    create_local_config(&temp_dir, "test-local-owner", "test-local-repo");

    let output = Command::new(devboy_bin())
        .args(["test", "github"])
        .env("DEVBOY_SKIP_KEYCHAIN", "1")
        .current_dir(temp_dir.path())
        .output()
        .expect("Failed to execute command");

    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);

    // Should find GitHub config from local .devboy.toml
    // The output shows "Testing GitHub connection..." and "Repository: owner/repo"
    // proving config was loaded
    let config_was_loaded = stderr.contains("GitHub token not set")
        || stderr.contains("Failed to get token")
        || stdout.contains("Testing GitHub connection")
        || stdout.contains("test-local-owner/test-local-repo");

    assert!(
        config_was_loaded,
        "Should load local config. stdout: {}, stderr: {}",
        stdout, stderr
    );

    // Verify it's NOT saying "GitHub not configured"
    assert!(
        !stderr.contains("GitHub not configured"),
        "Should have found GitHub config from local .devboy.toml"
    );
}

// =============================================================================
// Tests for local config priority
// =============================================================================

/// Test that local `.devboy.toml` takes priority over global config.
///
/// This test creates both a local and global config with different values
/// and verifies the local config is used.
#[test]
fn test_local_config_takes_priority_over_global() {
    let temp_dir = TempDir::new().unwrap();

    // Create a fake global config directory structure
    let fake_home = TempDir::new().unwrap();

    // Create global config directory with platform-specific path
    #[cfg(target_os = "windows")]
    let global_config_dir = fake_home.path().join("devboy-tools");
    #[cfg(not(target_os = "windows"))]
    let global_config_dir = fake_home.path().join(".config").join("devboy-tools");

    fs::create_dir_all(&global_config_dir).unwrap();

    // Write global config with different values (no github section)
    let global_config = r#"# Global config without github
"#;
    fs::write(global_config_dir.join("config.toml"), global_config).unwrap();

    // Write local config with github section
    let local_config = r#"[github]
owner = "local-priority-owner"
repo = "local-priority-repo"
"#;
    fs::write(temp_dir.path().join(".devboy.toml"), local_config).unwrap();

    let mut cmd = Command::new(devboy_bin());
    cmd.args(["issues"])
        .env("DEVBOY_SKIP_KEYCHAIN", "1")
        .env("HOME", fake_home.path())
        .env("USERPROFILE", fake_home.path())
        .current_dir(temp_dir.path());

    // On Windows, also set APPDATA for dirs::config_dir() to work
    #[cfg(target_os = "windows")]
    cmd.env("APPDATA", fake_home.path());

    // On Unix, set XDG_CONFIG_HOME for completeness
    #[cfg(not(target_os = "windows"))]
    cmd.env("XDG_CONFIG_HOME", fake_home.path().join(".config"));

    let output = cmd.output().expect("Failed to execute command");

    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);

    // The command should use LOCAL config (which has github section)
    // rather than global config (which has no github section).
    // If global was used, we'd see "No provider configured".
    // If local was used, we'd see a token/API error.
    let local_config_was_used = stderr.contains("GitHub token not set")
        || stderr.contains("Failed to get token")
        || stderr.contains("Failed to fetch issues")
        || stderr.contains("404");

    assert!(
        local_config_was_used,
        "Should use local config (with github section), not global. stdout: {}, stderr: {}",
        stdout, stderr
    );

    // This is the key assertion: if "No provider configured" appears,
    // it means global config was used instead of local
    assert!(
        !stdout.contains("No provider configured"),
        "Local config should take priority over global config"
    );
}
