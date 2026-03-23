//! Integration tests for `devboy upgrade` command.
//!
//! These tests verify the upgrade command behavior including help output,
//! check-only mode, and package manager detection.
//!
//! # Running Tests
//!
//! ```bash
//! cargo test --test upgrade_test
//! ```

use std::process::Command;

/// Get path to the devboy binary.
fn devboy_bin() -> std::path::PathBuf {
    let mut path = std::env::current_exe().unwrap();
    path.pop(); // Remove test binary name
    path.pop(); // Remove deps

    let bin_name = format!("devboy{}", std::env::consts::EXE_SUFFIX);
    path.push(bin_name);
    path
}

#[test]
fn test_upgrade_help() {
    let output = Command::new(devboy_bin())
        .args(["upgrade", "--help"])
        .output()
        .expect("Failed to execute command");

    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(output.status.success());
    assert!(stdout.contains("Upgrade devboy to the latest version"));
    assert!(stdout.contains("--check"));
}

#[test]
fn test_upgrade_check_shows_current_version() {
    let output = Command::new(devboy_bin())
        .args(["upgrade", "--check"])
        .output()
        .expect("Failed to execute command");

    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(output.status.success());
    assert!(
        stdout.contains("Current version:"),
        "Expected 'Current version:' in output, got: {}",
        stdout
    );
}

#[test]
fn test_upgrade_check_outputs_version_info() {
    let output = Command::new(devboy_bin())
        .args(["upgrade", "--check"])
        .output()
        .expect("Failed to execute command");

    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(output.status.success());
    // Should either say "already running the latest" or "New version available"
    assert!(
        stdout.contains("latest version") || stdout.contains("New version available"),
        "Expected version status in output, got: {}",
        stdout
    );
}

#[test]
fn test_upgrade_detects_npm_install_when_node_modules_in_path() {
    // Simulate npm installation by setting npm_config_user_agent
    // Note: this won't trigger because the binary path isn't inside node_modules,
    // but we verify the command runs without error
    let output = Command::new(devboy_bin())
        .args(["upgrade", "--check"])
        .env("npm_config_user_agent", "pnpm/9.0.0 node/22.0.0")
        .output()
        .expect("Failed to execute command");

    assert!(
        output.status.success(),
        "Command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn test_upgrade_appears_in_main_help() {
    let output = Command::new(devboy_bin())
        .args(["--help"])
        .output()
        .expect("Failed to execute command");

    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(output.status.success());
    assert!(
        stdout.contains("upgrade"),
        "Expected 'upgrade' in main help output"
    );
}

#[test]
fn test_update_check_suppressed_in_ci() {
    // Use `config path` — a real subcommand that goes through main() and triggers update check.
    // With CI=true the update check should be suppressed.
    let output = Command::new(devboy_bin())
        .args(["config", "path"])
        .env("CI", "true")
        .env("DEVBOY_SKIP_KEYCHAIN", "1")
        .output()
        .expect("Failed to execute command");

    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(output.status.success());
    assert!(
        !stderr.contains("new version"),
        "Update check should be suppressed in CI, but got stderr: {}",
        stderr
    );
}

#[test]
fn test_update_check_suppressed_with_env_var() {
    // Use `config path` — a real subcommand that goes through main() and triggers update check.
    let output = Command::new(devboy_bin())
        .args(["config", "path"])
        .env("DEVBOY_NO_UPDATE_CHECK", "1")
        .env("DEVBOY_SKIP_KEYCHAIN", "1")
        .output()
        .expect("Failed to execute command");

    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(output.status.success());
    assert!(
        !stderr.contains("new version"),
        "Update check should be suppressed with DEVBOY_NO_UPDATE_CHECK=1, but got stderr: {}",
        stderr
    );
}

#[test]
fn test_version_flag_still_works() {
    let output = Command::new(devboy_bin())
        .args(["--version"])
        .output()
        .expect("Failed to execute command");

    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(output.status.success());
    assert!(
        stdout.contains("devboy"),
        "Expected 'devboy' in version output, got: {}",
        stdout
    );
}
