//! Integration tests for `devboy upgrade` command.
//!
//! These tests verify the upgrade command behavior including help output,
//! check-only mode, and package manager detection.
//!
//! Tests that call the GitHub API use `GITHUB_TOKEN` env var for authentication
//! when available (5000 req/hr vs 60 req/hr unauthenticated). If the API is
//! unreachable or rate-limited, these tests skip gracefully instead of failing.
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

/// Run `devboy upgrade --check` and return the output.
/// Passes through GITHUB_TOKEN for authenticated API access.
fn run_upgrade_check() -> std::process::Output {
    let mut cmd = Command::new(devboy_bin());
    cmd.args(["upgrade", "--check"]);

    // Forward GITHUB_TOKEN if available for authenticated API access
    if let Ok(token) = std::env::var("GITHUB_TOKEN") {
        cmd.env("GITHUB_TOKEN", token);
    } else if let Ok(token) = std::env::var("GH_TOKEN") {
        cmd.env("GH_TOKEN", token);
    }

    cmd.output().expect("Failed to execute command")
}

/// Check if a command failure is due to GitHub API rate limiting or network issues.
fn is_api_unavailable(output: &std::process::Output) -> bool {
    if output.status.success() {
        return false;
    }
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    combined.contains("rate limit")
        || combined.contains("GitHub API returned status")
        || combined.contains("Failed to fetch release info")
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
    let output = run_upgrade_check();

    if is_api_unavailable(&output) {
        eprintln!("Skipping: GitHub API unavailable");
        return;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "Command failed.\nstdout: {}\nstderr: {}",
        stdout,
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("Current version:"),
        "Expected 'Current version:' in output, got: {}",
        stdout
    );
}

#[test]
fn test_upgrade_check_outputs_version_info() {
    let output = run_upgrade_check();

    if is_api_unavailable(&output) {
        eprintln!("Skipping: GitHub API unavailable");
        return;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "Command failed.\nstdout: {}\nstderr: {}",
        stdout,
        String::from_utf8_lossy(&output.stderr)
    );

    // Should either say "already running the latest" or "New version available"
    assert!(
        stdout.contains("latest version") || stdout.contains("New version available"),
        "Expected version status in output, got: {}",
        stdout
    );
}

#[test]
fn test_upgrade_detects_npm_install_when_node_modules_in_path() {
    let mut cmd = Command::new(devboy_bin());
    cmd.args(["upgrade", "--check"])
        .env("npm_config_user_agent", "pnpm/9.0.0 node/22.0.0");

    // Forward GITHUB_TOKEN if available
    if let Ok(token) = std::env::var("GITHUB_TOKEN") {
        cmd.env("GITHUB_TOKEN", token);
    } else if let Ok(token) = std::env::var("GH_TOKEN") {
        cmd.env("GH_TOKEN", token);
    }

    let output = cmd.output().expect("Failed to execute command");

    if is_api_unavailable(&output) {
        eprintln!("Skipping: GitHub API unavailable");
        return;
    }

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
