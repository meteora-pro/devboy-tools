//! Integration tests for local `.devboy.toml` configuration loading.
//!
//! These tests verify that commands correctly use local `.devboy.toml` when present,
//! falling back to global config otherwise.
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

#[test]
fn test_issues_uses_local_config() {
    let temp_dir = TempDir::new().unwrap();

    // Create local config with specific owner/repo
    create_local_config(&temp_dir, "local-owner", "local-repo");

    let output = Command::new(devboy_bin())
        .args(["issues"])
        // Skip keychain operations - will fail on token lookup, but we can verify config is loaded
        .env("DEVBOY_SKIP_KEYCHAIN", "1")
        .current_dir(temp_dir.path())
        .output()
        .expect("Failed to execute command");

    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);

    // The command will fail because there's no token OR because the repo doesn't exist.
    // What matters is that it tried to use the LOCAL config (not global).
    // If it used Config::load() (global), it would either:
    // - Say "No provider configured" (if global config has no github section)
    // - Use global owner/repo values
    // With load_runtime_config(), it should use local-owner/local-repo

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

#[test]
fn test_issues_without_local_config_uses_global() {
    let temp_dir = TempDir::new().unwrap();

    // No local .devboy.toml - should use global config

    let output = Command::new(devboy_bin())
        .args(["issues"])
        .env("DEVBOY_SKIP_KEYCHAIN", "1")
        // Use fake HOME to avoid loading real global config
        .env("HOME", temp_dir.path())
        .env("USERPROFILE", temp_dir.path())
        .current_dir(temp_dir.path())
        .output()
        .expect("Failed to execute command");

    let stdout = String::from_utf8_lossy(&output.stdout);

    // Without any config (local or global), should say "No provider configured"
    assert!(
        stdout.contains("No provider configured"),
        "Should indicate no provider is configured, got stdout: {}, stderr: {}",
        stdout,
        String::from_utf8_lossy(&output.stderr)
    );
}

// =============================================================================
// Tests for `mrs` command using local config
// =============================================================================

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

#[test]
fn test_mrs_without_local_config_uses_global() {
    let temp_dir = TempDir::new().unwrap();

    let output = Command::new(devboy_bin())
        .args(["mrs"])
        .env("DEVBOY_SKIP_KEYCHAIN", "1")
        .env("HOME", temp_dir.path())
        .env("USERPROFILE", temp_dir.path())
        .current_dir(temp_dir.path())
        .output()
        .expect("Failed to execute command");

    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        stdout.contains("No provider configured"),
        "Should indicate no provider is configured, got stdout: {}, stderr: {}",
        stdout,
        String::from_utf8_lossy(&output.stderr)
    );
}

// =============================================================================
// Tests for `test` command using local config
// =============================================================================

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

#[test]
fn test_test_command_without_local_config_uses_global() {
    let temp_dir = TempDir::new().unwrap();

    let output = Command::new(devboy_bin())
        .args(["test", "github"])
        .env("DEVBOY_SKIP_KEYCHAIN", "1")
        .env("HOME", temp_dir.path())
        .env("USERPROFILE", temp_dir.path())
        .current_dir(temp_dir.path())
        .output()
        .expect("Failed to execute command");

    let stderr = String::from_utf8_lossy(&output.stderr);

    // Without any config, should say GitHub not configured
    assert!(
        stderr.contains("GitHub not configured"),
        "Should indicate GitHub not configured, got stderr: {}",
        stderr
    );
}

// =============================================================================
// Tests for local config priority
// =============================================================================

#[test]
fn test_local_config_takes_priority_over_global() {
    let temp_dir = TempDir::new().unwrap();

    // Create a fake global config directory
    let fake_home = TempDir::new().unwrap();
    let global_config_dir = fake_home.path().join(".config").join("devboy");
    fs::create_dir_all(&global_config_dir).unwrap();

    // Write global config with different values
    let global_config = r#"[github]
owner = "global-owner"
repo = "global-repo"
"#;
    fs::write(global_config_dir.join("config.toml"), global_config).unwrap();

    // Write local config with different values
    let local_config = r#"[github]
owner = "local-priority-owner"
repo = "local-priority-repo"
"#;
    fs::write(temp_dir.path().join(".devboy.toml"), local_config).unwrap();

    let output = Command::new(devboy_bin())
        .args(["issues"])
        .env("DEVBOY_SKIP_KEYCHAIN", "1")
        .env("HOME", fake_home.path())
        .env("USERPROFILE", fake_home.path())
        .current_dir(temp_dir.path())
        .output()
        .expect("Failed to execute command");

    let stderr = String::from_utf8_lossy(&output.stderr);

    // The command should try to use LOCAL config (which has github section)
    // rather than global config. Both have github, but we expect local to win.
    // Since we can't directly verify which owner/repo was used without a working API,
    // we verify that GitHub config was found (proving config loading worked).
    assert!(
        stderr.contains("GitHub token not set") || stderr.contains("Failed to get token"),
        "Should load config (local should have priority), got: {}",
        stderr
    );
}
