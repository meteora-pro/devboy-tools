//! Integration tests for `devboy init` command.
//!
//! These tests verify the init command behavior in non-interactive mode.
//! Interactive mode testing would require stdin mocking which is out of scope.
//!
//! # Running Tests
//!
//! ```bash
//! cargo test --test init_test
//! ```

use std::fs;
use std::process::Command;
use tempfile::TempDir;

/// Get path to the devboy binary.
fn devboy_bin() -> std::path::PathBuf {
    // Use debug build for tests
    let mut path = std::env::current_exe().unwrap();
    path.pop(); // Remove test binary name
    path.pop(); // Remove deps
    path.push("devboy");
    path
}

/// Create a temporary directory with a git repository initialized.
fn create_temp_git_repo(remote_url: &str) -> TempDir {
    let temp_dir = TempDir::new().unwrap();

    // Initialize git repo
    Command::new("git")
        .args(["init"])
        .current_dir(temp_dir.path())
        .output()
        .expect("Failed to init git repo");

    // Add remote
    Command::new("git")
        .args(["remote", "add", "origin", remote_url])
        .current_dir(temp_dir.path())
        .output()
        .expect("Failed to add git remote");

    temp_dir
}

#[test]
fn test_init_help() {
    let output = Command::new(devboy_bin())
        .args(["init", "--help"])
        .output()
        .expect("Failed to execute command");

    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(output.status.success());
    assert!(stdout.contains("--yes"));
    assert!(stdout.contains("--dry-run"));
    assert!(stdout.contains("--force"));
    assert!(stdout.contains("--claude"));
    assert!(stdout.contains("--context"));
}

#[test]
fn test_init_dry_run_creates_no_files() {
    let temp_dir = create_temp_git_repo("git@github.com:test-owner/test-repo.git");
    let config_path = temp_dir.path().join(".devboy.toml");

    let output = Command::new(devboy_bin())
        .args(["init", "--yes", "--dry-run"])
        .current_dir(temp_dir.path())
        .output()
        .expect("Failed to execute command");

    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(output.status.success(), "Command should succeed");
    assert!(stdout.contains("[dry-run]"), "Should indicate dry-run mode");
    assert!(stdout.contains("Would create"), "Should say would create");
    assert!(
        !config_path.exists(),
        "Config file should NOT be created in dry-run mode"
    );
}

#[test]
fn test_init_yes_creates_config_with_github() {
    let temp_dir = create_temp_git_repo("git@github.com:test-owner/test-repo.git");
    let config_path = temp_dir.path().join(".devboy.toml");

    let output = Command::new(devboy_bin())
        .args(["init", "--yes"])
        .current_dir(temp_dir.path())
        .output()
        .expect("Failed to execute command");

    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(output.status.success(), "Command should succeed");
    assert!(
        stdout.contains("Detected GitHub repository"),
        "Should detect GitHub"
    );
    assert!(config_path.exists(), "Config file should be created");

    // Verify config content
    let content = fs::read_to_string(&config_path).unwrap();
    assert!(content.contains("github"), "Should contain github section");
    assert!(content.contains("test-owner"), "Should contain owner");
    assert!(content.contains("test-repo"), "Should contain repo");
}

#[test]
fn test_init_yes_creates_config_with_gitlab() {
    let temp_dir = create_temp_git_repo("git@gitlab.com:company/project.git");
    let config_path = temp_dir.path().join(".devboy.toml");

    let output = Command::new(devboy_bin())
        .args(["init", "--yes"])
        .current_dir(temp_dir.path())
        .output()
        .expect("Failed to execute command");

    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(output.status.success(), "Command should succeed");
    assert!(
        stdout.contains("Detected GitLab repository"),
        "Should detect GitLab"
    );
    assert!(config_path.exists(), "Config file should be created");

    // Verify config content
    let content = fs::read_to_string(&config_path).unwrap();
    assert!(content.contains("gitlab"), "Should contain gitlab section");
    assert!(
        content.contains("company/project"),
        "Should contain project path"
    );
}

#[test]
fn test_init_yes_with_https_remote() {
    let temp_dir = create_temp_git_repo("https://github.com/https-owner/https-repo.git");
    let config_path = temp_dir.path().join(".devboy.toml");

    let output = Command::new(devboy_bin())
        .args(["init", "--yes"])
        .current_dir(temp_dir.path())
        .output()
        .expect("Failed to execute command");

    assert!(output.status.success(), "Command should succeed");
    assert!(config_path.exists(), "Config file should be created");

    let content = fs::read_to_string(&config_path).unwrap();
    assert!(
        content.contains("https-owner"),
        "Should parse HTTPS remote correctly"
    );
    assert!(content.contains("https-repo"), "Should parse repo name");
}

#[test]
fn test_init_custom_context_name() {
    let temp_dir = create_temp_git_repo("git@github.com:owner/repo.git");
    let config_path = temp_dir.path().join(".devboy.toml");

    let output = Command::new(devboy_bin())
        .args(["init", "--yes", "--context", "my-custom-context"])
        .current_dir(temp_dir.path())
        .output()
        .expect("Failed to execute command");

    assert!(output.status.success(), "Command should succeed");

    let content = fs::read_to_string(&config_path).unwrap();
    assert!(
        content.contains("my-custom-context"),
        "Should use custom context name"
    );
}

#[test]
fn test_init_fails_if_config_exists_without_force() {
    let temp_dir = create_temp_git_repo("git@github.com:owner/repo.git");
    let config_path = temp_dir.path().join(".devboy.toml");

    // Create existing config
    fs::write(&config_path, "# existing config\n").unwrap();

    let output = Command::new(devboy_bin())
        .args(["init", "--yes"])
        .current_dir(temp_dir.path())
        .output()
        .expect("Failed to execute command");

    assert!(
        !output.status.success(),
        "Command should fail when config exists"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("already exists") || stderr.contains("--force"),
        "Should mention config exists or suggest --force"
    );
}

#[test]
fn test_init_force_creates_backup() {
    let temp_dir = create_temp_git_repo("git@github.com:owner/repo.git");
    let config_path = temp_dir.path().join(".devboy.toml");

    // Create existing config
    let original_content = "# original config\n[contexts.old]\n";
    fs::write(&config_path, original_content).unwrap();

    let output = Command::new(devboy_bin())
        .args(["init", "--yes", "--force"])
        .current_dir(temp_dir.path())
        .output()
        .expect("Failed to execute command");

    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        output.status.success(),
        "Command should succeed with --force"
    );
    assert!(stdout.contains("backup"), "Should mention backup creation");

    // Verify backup exists
    let entries: Vec<_> = fs::read_dir(temp_dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with(".devboy.toml.backup")
        })
        .collect();

    assert_eq!(entries.len(), 1, "Should have exactly one backup file");

    // Verify backup content matches original
    let backup_content = fs::read_to_string(entries[0].path()).unwrap();
    assert_eq!(
        backup_content, original_content,
        "Backup should contain original content"
    );

    // Verify new config is different
    let new_content = fs::read_to_string(&config_path).unwrap();
    assert_ne!(
        new_content, original_content,
        "New config should be different from original"
    );
}

#[test]
fn test_init_no_git_remote_creates_empty_config() {
    let temp_dir = TempDir::new().unwrap();

    // Initialize git repo without remote
    Command::new("git")
        .args(["init"])
        .current_dir(temp_dir.path())
        .output()
        .expect("Failed to init git repo");

    let output = Command::new(devboy_bin())
        .args(["init", "--yes"])
        .current_dir(temp_dir.path())
        .output()
        .expect("Failed to execute command");

    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(output.status.success(), "Command should succeed");
    assert!(
        stdout.contains("No git remote detected"),
        "Should indicate no remote found"
    );
}

#[test]
fn test_init_dry_run_with_force_shows_would_backup() {
    let temp_dir = create_temp_git_repo("git@github.com:owner/repo.git");
    let config_path = temp_dir.path().join(".devboy.toml");

    // Create existing config
    fs::write(&config_path, "# existing\n").unwrap();

    let output = Command::new(devboy_bin())
        .args(["init", "--yes", "--dry-run", "--force"])
        .current_dir(temp_dir.path())
        .output()
        .expect("Failed to execute command");

    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(output.status.success(), "Command should succeed");
    assert!(stdout.contains("[dry-run]"), "Should be in dry-run mode");

    // Verify no backup was actually created
    let backups: Vec<_> = fs::read_dir(temp_dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().contains(".backup"))
        .collect();

    assert!(
        backups.is_empty(),
        "No backup should be created in dry-run mode"
    );
}

#[test]
fn test_init_unknown_provider_no_config() {
    let temp_dir = create_temp_git_repo("git@bitbucket.org:owner/repo.git");

    let output = Command::new(devboy_bin())
        .args(["init", "--yes"])
        .current_dir(temp_dir.path())
        .output()
        .expect("Failed to execute command");

    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(output.status.success(), "Command should succeed");
    assert!(
        stdout.contains("No git remote detected") || stdout.contains("minimal config"),
        "Should indicate unknown provider or minimal config"
    );
}
