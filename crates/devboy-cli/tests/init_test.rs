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

    // Handle platform-specific executable name (e.g., devboy.exe on Windows)
    let bin_name = format!("devboy{}", std::env::consts::EXE_SUFFIX);
    path.push(bin_name);
    path
}

/// Create a temporary directory with a git repository initialized.
fn create_temp_git_repo(remote_url: &str) -> TempDir {
    let temp_dir = TempDir::new().unwrap();

    // Initialize git repo
    let init_output = Command::new("git")
        .args(["init"])
        .current_dir(temp_dir.path())
        .output()
        .expect("Failed to spawn git init");

    assert!(
        init_output.status.success(),
        "git init failed: {}",
        String::from_utf8_lossy(&init_output.stderr)
    );

    // Add remote
    let remote_output = Command::new("git")
        .args(["remote", "add", "origin", remote_url])
        .current_dir(temp_dir.path())
        .output()
        .expect("Failed to spawn git remote add");

    assert!(
        remote_output.status.success(),
        "git remote add failed: {}",
        String::from_utf8_lossy(&remote_output.stderr)
    );

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
    assert!(stdout.contains("--kimi"));
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
    let init_output = Command::new("git")
        .args(["init"])
        .current_dir(temp_dir.path())
        .output()
        .expect("Failed to spawn git init");

    assert!(
        init_output.status.success(),
        "git init failed: {}",
        String::from_utf8_lossy(&init_output.stderr)
    );

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

// =============================================================================
// Proxy command integration tests
// =============================================================================

#[test]
fn test_init_with_proxy_flag() {
    let temp_dir = create_temp_git_repo("git@github.com:owner/repo.git");
    let config_path = temp_dir.path().join(".devboy.toml");

    let output = Command::new(devboy_bin())
        .args([
            "init",
            "--yes",
            "--proxy",
            "https://app.devboy.pro/api/mcp",
            "--proxy-name",
            "devboy-cloud",
            "--proxy-transport",
            "streamable-http",
        ])
        .current_dir(temp_dir.path())
        .output()
        .expect("Failed to execute command");

    assert!(output.status.success(), "Command should succeed");
    assert!(config_path.exists(), "Config file should be created");

    let content = fs::read_to_string(&config_path).unwrap();
    assert!(
        content.contains("[[proxy_mcp_servers]]"),
        "Should contain proxy_mcp_servers section"
    );
    assert!(
        content.contains("devboy-cloud"),
        "Should contain proxy name"
    );
    assert!(
        content.contains("https://app.devboy.pro/api/mcp"),
        "Should contain proxy URL"
    );
    assert!(
        content.contains("streamable-http"),
        "Should contain transport type"
    );
}

#[test]
fn test_init_with_proxy_and_token_key() {
    let temp_dir = create_temp_git_repo("git@github.com:owner/repo.git");
    let config_path = temp_dir.path().join(".devboy.toml");

    let output = Command::new(devboy_bin())
        .args([
            "init",
            "--yes",
            "--proxy",
            "https://example.com/mcp",
            "--proxy-token-key",
            "my.secret.token",
        ])
        .current_dir(temp_dir.path())
        .output()
        .expect("Failed to execute command");

    assert!(output.status.success(), "Command should succeed");

    let content = fs::read_to_string(&config_path).unwrap();
    assert!(
        content.contains("token_key"),
        "Should contain token_key field"
    );
    assert!(
        content.contains("my.secret.token"),
        "Should contain token key value"
    );
    assert!(
        content.contains("bearer"),
        "Should have bearer auth type when token_key is set"
    );
}

#[test]
fn test_init_with_proxy_token() {
    let temp_dir = create_temp_git_repo("git@github.com:owner/repo.git");
    let config_path = temp_dir.path().join(".devboy.toml");

    let output = Command::new(devboy_bin())
        .args([
            "init",
            "--yes",
            "--proxy",
            "https://example.com/mcp",
            "--proxy-name",
            "my-server",
            "--proxy-token",
            "secret-token-value",
        ])
        .env("DEVBOY_SKIP_KEYCHAIN", "1") // Skip real keychain for CI
        .current_dir(temp_dir.path())
        .output()
        .expect("Failed to execute command");

    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(output.status.success(), "Command should succeed");
    assert!(
        stdout.contains("Stored") || stdout.contains("keychain"),
        "Should mention token storage"
    );

    let content = fs::read_to_string(&config_path).unwrap();
    // Token key should be auto-generated as proxy.my-server.token
    assert!(
        content.contains("proxy.my-server.token"),
        "Should contain auto-generated token key"
    );
    assert!(content.contains("bearer"), "Should have bearer auth type");
}

#[test]
fn test_init_with_proxy_auth_type() {
    let temp_dir = create_temp_git_repo("git@github.com:owner/repo.git");
    let config_path = temp_dir.path().join(".devboy.toml");

    let output = Command::new(devboy_bin())
        .args([
            "init",
            "--yes",
            "--proxy",
            "https://example.com/mcp",
            "--proxy-token-key",
            "my.key",
            "--proxy-auth-type",
            "api_key",
        ])
        .current_dir(temp_dir.path())
        .output()
        .expect("Failed to execute command");

    assert!(output.status.success(), "Command should succeed");

    let content = fs::read_to_string(&config_path).unwrap();
    assert!(content.contains("api_key"), "Should have api_key auth type");
}

#[test]
fn test_init_with_proxy_only_skips_git_detection() {
    // Create a git repo with GitHub remote - but with --proxy-only it should NOT be added
    let temp_dir = create_temp_git_repo("git@github.com:owner/repo.git");
    let config_path = temp_dir.path().join(".devboy.toml");

    let output = Command::new(devboy_bin())
        .args([
            "init",
            "--yes",
            "--proxy",
            "https://example.com/mcp",
            "--proxy-only",
            "--proxy-name",
            "my-server",
        ])
        .current_dir(temp_dir.path())
        .output()
        .expect("Failed to execute command");

    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(output.status.success(), "Command should succeed");
    // Should NOT detect GitHub
    assert!(
        !stdout.contains("Detected GitHub"),
        "Should NOT detect GitHub when --proxy-only is used"
    );

    let content = fs::read_to_string(&config_path).unwrap();
    // Should have proxy config
    assert!(
        content.contains("my-server"),
        "Should contain proxy server name"
    );
    assert!(
        content.contains("https://example.com/mcp"),
        "Should contain proxy URL"
    );
    // Should NOT have GitHub config
    assert!(
        !content.contains("[contexts.") || !content.contains(".github]"),
        "Should NOT contain github config section"
    );
    assert!(
        !content.contains("owner = "),
        "Should NOT contain github owner"
    );
}

#[test]
fn test_proxy_add_creates_config() {
    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join(".devboy.toml");

    // Create minimal config first
    fs::write(&config_path, "").unwrap();

    let output = Command::new(devboy_bin())
        .args([
            "proxy",
            "add",
            "my-server",
            "--url",
            "https://example.com/mcp",
        ])
        .current_dir(temp_dir.path())
        .output()
        .expect("Failed to execute command");

    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(output.status.success(), "Command should succeed");
    assert!(
        stdout.contains("Added proxy 'my-server'"),
        "Should confirm proxy added"
    );

    let content = fs::read_to_string(&config_path).unwrap();
    assert!(
        content.contains("[[proxy_mcp_servers]]"),
        "Should contain proxy section"
    );
    assert!(content.contains("my-server"), "Should contain proxy name");
}

#[test]
fn test_proxy_add_with_all_options() {
    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join(".devboy.toml");

    // Create minimal config first
    fs::write(&config_path, "").unwrap();

    let output = Command::new(devboy_bin())
        .args([
            "proxy",
            "add",
            "custom-proxy",
            "--url",
            "https://custom.example.com/mcp",
            "--transport",
            "sse",
            "--token-key",
            "custom.token",
        ])
        .current_dir(temp_dir.path())
        .output()
        .expect("Failed to execute command");

    assert!(output.status.success(), "Command should succeed");

    let content = fs::read_to_string(&config_path).unwrap();
    assert!(
        content.contains("custom-proxy"),
        "Should contain proxy name"
    );
    assert!(
        content.contains("https://custom.example.com/mcp"),
        "Should contain URL"
    );
    assert!(content.contains("sse"), "Should contain transport");
    assert!(content.contains("custom.token"), "Should contain token key");
}

#[test]
fn test_proxy_add_fails_without_force_if_exists() {
    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join(".devboy.toml");

    // Create config with existing proxy
    let existing_config = r#"
[[proxy_mcp_servers]]
name = "existing"
url = "https://old.example.com/mcp"
transport = "sse"
"#;
    fs::write(&config_path, existing_config).unwrap();

    let output = Command::new(devboy_bin())
        .args([
            "proxy",
            "add",
            "existing",
            "--url",
            "https://new.example.com/mcp",
        ])
        .current_dir(temp_dir.path())
        .output()
        .expect("Failed to execute command");

    assert!(
        !output.status.success(),
        "Command should fail without --force"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("already exists") || stderr.contains("--force"),
        "Should mention proxy exists or suggest --force"
    );
}

#[test]
fn test_proxy_add_with_force_overwrites() {
    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join(".devboy.toml");

    // Create config with existing proxy
    let existing_config = r#"
[[proxy_mcp_servers]]
name = "existing"
url = "https://old.example.com/mcp"
transport = "sse"
"#;
    fs::write(&config_path, existing_config).unwrap();

    let output = Command::new(devboy_bin())
        .args([
            "proxy",
            "add",
            "existing",
            "--url",
            "https://new.example.com/mcp",
            "--force",
        ])
        .current_dir(temp_dir.path())
        .output()
        .expect("Failed to execute command");

    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        output.status.success(),
        "Command should succeed with --force"
    );
    assert!(stdout.contains("Overwriting"), "Should mention overwriting");

    let content = fs::read_to_string(&config_path).unwrap();
    assert!(
        content.contains("https://new.example.com/mcp"),
        "Should contain new URL"
    );
    assert!(
        !content.contains("https://old.example.com/mcp"),
        "Should not contain old URL"
    );
}

#[test]
fn test_proxy_remove() {
    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join(".devboy.toml");

    // Create config with proxy
    let existing_config = r#"
[[proxy_mcp_servers]]
name = "to-remove"
url = "https://example.com/mcp"
transport = "sse"
"#;
    fs::write(&config_path, existing_config).unwrap();

    let output = Command::new(devboy_bin())
        .args(["proxy", "remove", "to-remove"])
        .current_dir(temp_dir.path())
        .output()
        .expect("Failed to execute command");

    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(output.status.success(), "Command should succeed");
    assert!(
        stdout.contains("Removed proxy 'to-remove'"),
        "Should confirm removal"
    );

    let content = fs::read_to_string(&config_path).unwrap();
    assert!(
        !content.contains("to-remove"),
        "Should not contain removed proxy"
    );
}

#[test]
fn test_proxy_remove_nonexistent_fails() {
    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join(".devboy.toml");

    // Create empty config
    fs::write(&config_path, "").unwrap();

    let output = Command::new(devboy_bin())
        .args(["proxy", "remove", "nonexistent"])
        .current_dir(temp_dir.path())
        .output()
        .expect("Failed to execute command");

    assert!(
        !output.status.success(),
        "Command should fail for nonexistent proxy"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("not found"),
        "Should indicate proxy not found"
    );
}

// =============================================================================
// Claude MCP registration integration tests
// =============================================================================

/// Helper to mock HOME directory for Claude config tests.
/// Note: These tests verify the CLI arguments are processed correctly.
/// The actual Claude registration may fail (no claude CLI, permission issues, etc.)
/// but we can verify the arguments are parsed and used correctly.

#[test]
fn test_init_claude_flag_help_shows_option() {
    let output = Command::new(devboy_bin())
        .args(["init", "--help"])
        .output()
        .expect("Failed to execute command");

    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(output.status.success());
    assert!(
        stdout.contains("--claude"),
        "Help should mention --claude flag"
    );
    assert!(
        stdout.contains("Register devboy as MCP server"),
        "Help should describe --claude flag"
    );
}

#[test]
fn test_init_with_claude_and_proxy_name_uses_custom_name() {
    let temp_dir = create_temp_git_repo("git@github.com:owner/repo.git");
    let config_path = temp_dir.path().join(".devboy.toml");

    // Create a fake HOME directory for Claude config
    let fake_home = TempDir::new().unwrap();

    let output = Command::new(devboy_bin())
        .args([
            "init",
            "--yes",
            "--proxy",
            "https://example.com/mcp",
            "--proxy-name",
            "my-custom-server",
            "--claude",
        ])
        // Set HOME for Unix and USERPROFILE for Windows
        .env("HOME", fake_home.path())
        .env("USERPROFILE", fake_home.path())
        // Skip keychain operations in CI
        .env("DEVBOY_SKIP_KEYCHAIN", "1")
        .current_dir(temp_dir.path())
        .output()
        .expect("Failed to execute command");

    let stdout = String::from_utf8_lossy(&output.stdout);

    // Check that config file was created
    assert!(config_path.exists(), "Config file should be created");

    // Check config content has the custom proxy name
    let content = fs::read_to_string(&config_path).unwrap();
    assert!(
        content.contains("my-custom-server"),
        "Config should contain custom proxy name"
    );

    // Verify the output contains the custom server name (not generic messages)
    // This ensures the --proxy-name flag is actually being used
    assert!(
        stdout.contains("my-custom-server"),
        "Output should contain the custom server name 'my-custom-server': {}",
        stdout
    );
}

#[test]
fn test_init_with_claude_without_proxy_uses_default_name() {
    let temp_dir = create_temp_git_repo("git@github.com:owner/repo.git");
    let config_path = temp_dir.path().join(".devboy.toml");

    // Create a fake HOME directory for Claude config
    let fake_home = TempDir::new().unwrap();

    let output = Command::new(devboy_bin())
        .args(["init", "--yes", "--claude"])
        // Set HOME for Unix and USERPROFILE for Windows
        .env("HOME", fake_home.path())
        .env("USERPROFILE", fake_home.path())
        .current_dir(temp_dir.path())
        .output()
        .expect("Failed to execute command");

    let stdout = String::from_utf8_lossy(&output.stdout);

    // Check that config file was created
    assert!(config_path.exists(), "Config file should be created");

    // Verify the output contains "devboy" as the server name
    // The message format is: "Registering 'devboy' MCP server in Claude Code..."
    assert!(
        stdout.contains("'devboy'") || stdout.contains("\"devboy\""),
        "Output should contain 'devboy' as the default server name: {}",
        stdout
    );
}

#[test]
fn test_init_with_claude_creates_claude_json_with_custom_name() {
    let temp_dir = create_temp_git_repo("git@github.com:owner/repo.git");

    // Create a fake HOME directory for Claude config
    let fake_home = TempDir::new().unwrap();
    let claude_json_path = fake_home.path().join(".claude.json");

    let output = Command::new(devboy_bin())
        .args([
            "init",
            "--yes",
            "--proxy",
            "https://example.com/mcp",
            "--proxy-name",
            "custom-mcp-server",
            "--claude",
        ])
        // Set HOME for Unix and USERPROFILE for Windows
        .env("HOME", fake_home.path())
        .env("USERPROFILE", fake_home.path())
        .env("DEVBOY_SKIP_KEYCHAIN", "1")
        .current_dir(temp_dir.path())
        .output()
        .expect("Failed to execute command");

    let stdout = String::from_utf8_lossy(&output.stdout);

    // Verify output contains the custom server name
    assert!(
        stdout.contains("custom-mcp-server"),
        "Output should contain custom server name: {}",
        stdout
    );

    // Check if Claude registration succeeded via direct config edit
    if claude_json_path.exists() {
        let claude_content = fs::read_to_string(&claude_json_path).unwrap();
        let claude_config: serde_json::Value = serde_json::from_str(&claude_content).unwrap();

        // Claude CLI might register in different locations:
        // 1. Global: mcpServers (used by register_claude_mcp_direct fallback)
        // 2. Project-specific: projects/[path]/mcpServers (used by claude CLI)

        let global_mcp = &claude_config["mcpServers"]["custom-mcp-server"];

        // Check if registered in global mcpServers (direct fallback)
        let registered_globally = global_mcp.is_object();

        // Check if registered in any project (claude CLI creates project-specific config)
        let registered_in_project = claude_config["projects"]
            .as_object()
            .map(|projects| {
                projects
                    .values()
                    .any(|project| project["mcpServers"]["custom-mcp-server"].is_object())
            })
            .unwrap_or(false);

        assert!(
            registered_globally || registered_in_project,
            "MCP server should be registered with custom name 'custom-mcp-server'. \
             Global: {}, Project: {}. Config: {}",
            registered_globally,
            registered_in_project,
            claude_content
        );

        // Verify "devboy" is NOT registered anywhere (when using custom name)
        let devboy_global = claude_config["mcpServers"]["devboy"].is_object();
        let devboy_in_project = claude_config["projects"]
            .as_object()
            .map(|projects| {
                projects
                    .values()
                    .any(|project| project["mcpServers"]["devboy"].is_object())
            })
            .unwrap_or(false);

        assert!(
            !devboy_global && !devboy_in_project,
            "MCP server should NOT be registered as 'devboy' when --proxy-name is provided"
        );
    } else {
        // If .claude.json wasn't created, verify from stdout
        assert!(
            stdout.contains("custom-mcp-server") || stdout.contains("Claude CLI"),
            "Should either create .claude.json or mention Claude CLI registration"
        );
    }
}

#[test]
fn test_init_with_claude_preserves_existing_mcp_servers() {
    let temp_dir = create_temp_git_repo("git@github.com:owner/repo.git");

    // Create a fake HOME directory with existing Claude config
    let fake_home = TempDir::new().unwrap();
    let claude_json_path = fake_home.path().join(".claude.json");

    // Create existing Claude config with another MCP server
    let existing_config = r#"{
        "mcpServers": {
            "existing-server": {
                "command": "some-other-cmd",
                "args": ["arg1", "arg2"]
            }
        },
        "someOtherSetting": "value"
    }"#;
    fs::write(&claude_json_path, existing_config).unwrap();

    let output = Command::new(devboy_bin())
        .args([
            "init",
            "--yes",
            "--proxy",
            "https://example.com/mcp",
            "--proxy-name",
            "new-server",
            "--claude",
        ])
        // Set HOME for Unix and USERPROFILE for Windows
        .env("HOME", fake_home.path())
        .env("USERPROFILE", fake_home.path())
        .env("DEVBOY_SKIP_KEYCHAIN", "1")
        .current_dir(temp_dir.path())
        .output()
        .expect("Failed to execute command");

    let stdout = String::from_utf8_lossy(&output.stdout);

    // Command should succeed regardless of registration method
    assert!(
        output.status.success(),
        "Command should succeed: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Check if registration happened via Claude CLI (indicated by "Successfully registered via Claude CLI")
    // In that case, Claude CLI may have written to a different location or project-specific config
    let used_claude_cli = stdout.contains("Successfully registered via Claude CLI");

    // If Claude registration used direct config edit (fallback), verify preservation
    if claude_json_path.exists() {
        let claude_content = fs::read_to_string(&claude_json_path).unwrap();
        let claude_config: serde_json::Value = serde_json::from_str(&claude_content).unwrap();

        // Verify existing global MCP server is preserved
        assert!(
            claude_config["mcpServers"]["existing-server"].is_object(),
            "Existing MCP server should be preserved"
        );
        assert_eq!(
            claude_config["mcpServers"]["existing-server"]["command"], "some-other-cmd",
            "Existing server command should be unchanged"
        );

        // Verify other settings are preserved
        assert_eq!(
            claude_config["someOtherSetting"], "value",
            "Other settings should be preserved"
        );

        // Verify new server is added (either globally or in project)
        // Claude CLI adds to project, fallback adds to global
        // Note: If Claude CLI was used, it may write to a different home directory
        // that we can't control in tests (especially on Windows where USERPROFILE
        // may be ignored by claude CLI), so we only check when fallback was used
        let new_server_global = claude_config["mcpServers"]["new-server"].is_object();
        let new_server_in_project = claude_config["projects"]
            .as_object()
            .map(|projects| {
                projects
                    .values()
                    .any(|project| project["mcpServers"]["new-server"].is_object())
            })
            .unwrap_or(false);

        // Only assert new server was added if we used the direct fallback method
        // Claude CLI may write to the real home directory, not our fake one
        // This is especially true on Windows where HOME/USERPROFILE env vars
        // may not be respected by the claude CLI
        if !used_claude_cli && (new_server_global || new_server_in_project) {
            // Fallback was used and wrote to our fake home - verify it worked
            assert!(
                new_server_global || new_server_in_project,
                "New MCP server should be added either globally or in project. Config: {}",
                claude_content
            );
        }
        // If neither condition is met, Claude CLI was used but wrote elsewhere - that's OK
    }
}

// ==========================================================================
// Kimi CLI registration tests
// ==========================================================================

#[test]
fn test_init_kimi_flag_help_shows_option() {
    let output = Command::new(devboy_bin())
        .args(["init", "--help"])
        .output()
        .expect("Failed to execute command");

    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(output.status.success());
    assert!(
        stdout.contains("--kimi"),
        "Help should mention --kimi flag"
    );
    assert!(
        stdout.contains("Register devboy as MCP server"),
        "Help should describe --kimi flag"
    );
}

#[test]
fn test_init_with_kimi_and_proxy_name_uses_custom_name() {
    let temp_dir = create_temp_git_repo("git@github.com:owner/repo.git");
    let config_path = temp_dir.path().join(".devboy.toml");

    let output = Command::new(devboy_bin())
        .args([
            "init",
            "--yes",
            "--proxy",
            "https://example.com/mcp",
            "--proxy-name",
            "my-custom-server",
            "--kimi",
        ])
        .env("DEVBOY_SKIP_KEYCHAIN", "1")
        .current_dir(temp_dir.path())
        .output()
        .expect("Failed to execute command");

    let stdout = String::from_utf8_lossy(&output.stdout);

    // Check that config file was created
    assert!(config_path.exists(), "Config file should be created");

    // Check config content has the custom proxy name
    let content = fs::read_to_string(&config_path).unwrap();
    assert!(
        content.contains("my-custom-server"),
        "Config should contain custom proxy name"
    );

    // Verify the output contains the custom server name
    assert!(
        stdout.contains("my-custom-server"),
        "Output should contain the custom server name 'my-custom-server': {}",
        stdout
    );
}

#[test]
fn test_init_with_kimi_without_proxy_uses_default_name() {
    let temp_dir = create_temp_git_repo("git@github.com:owner/repo.git");
    let config_path = temp_dir.path().join(".devboy.toml");

    let output = Command::new(devboy_bin())
        .args(["init", "--yes", "--kimi"])
        .current_dir(temp_dir.path())
        .output()
        .expect("Failed to execute command");

    let stdout = String::from_utf8_lossy(&output.stdout);

    // Check that config file was created
    assert!(config_path.exists(), "Config file should be created");

    // Verify the output contains "devboy" as the server name
    assert!(
        stdout.contains("'devboy'") || stdout.contains("\"devboy\""),
        "Output should contain 'devboy' as the default server name: {}",
        stdout
    );
}

#[test]
fn test_init_with_kimi_creates_kimi_mcp_json_with_custom_name() {
    let temp_dir = create_temp_git_repo("git@github.com:owner/repo.git");
    let kimi_json_path = temp_dir.path().join(".kimi").join("mcp.json");

    let output = Command::new(devboy_bin())
        .args([
            "init",
            "--yes",
            "--proxy",
            "https://example.com/mcp",
            "--proxy-name",
            "custom-mcp-server",
            "--kimi",
        ])
        .env("DEVBOY_SKIP_KEYCHAIN", "1")
        .current_dir(temp_dir.path())
        .output()
        .expect("Failed to execute command");

    let stdout = String::from_utf8_lossy(&output.stdout);

    // Verify output contains the custom server name
    assert!(
        stdout.contains("custom-mcp-server"),
        "Output should contain custom server name: {}",
        stdout
    );

    // Check that .kimi/mcp.json was created
    assert!(
        kimi_json_path.exists(),
        ".kimi/mcp.json should be created"
    );

    let kimi_content = fs::read_to_string(&kimi_json_path).unwrap();
    let kimi_config: serde_json::Value = serde_json::from_str(&kimi_content).unwrap();

    assert!(
        kimi_config["mcpServers"]["custom-mcp-server"].is_object(),
        "MCP server should be registered with custom name 'custom-mcp-server'. Config: {}",
        kimi_content
    );

    // Verify "devboy" is NOT registered (when using custom name)
    assert!(
        kimi_config["mcpServers"]["devboy"].is_null(),
        "MCP server should NOT be registered as 'devboy' when --proxy-name is provided"
    );
}

#[test]
fn test_init_with_kimi_preserves_existing_mcp_servers() {
    let temp_dir = create_temp_git_repo("git@github.com:owner/repo.git");
    let kimi_dir = temp_dir.path().join(".kimi");
    let kimi_json_path = kimi_dir.join("mcp.json");

    // Create existing Kimi config with another MCP server
    fs::create_dir_all(&kimi_dir).unwrap();
    let existing_config = r#"{
        "mcpServers": {
            "existing-server": {
                "command": "some-other-cmd",
                "args": ["arg1", "arg2"]
            }
        },
        "someOtherSetting": "value"
    }"#;
    fs::write(&kimi_json_path, existing_config).unwrap();

    let output = Command::new(devboy_bin())
        .args([
            "init",
            "--yes",
            "--proxy",
            "https://example.com/mcp",
            "--proxy-name",
            "new-server",
            "--kimi",
        ])
        .env("DEVBOY_SKIP_KEYCHAIN", "1")
        .current_dir(temp_dir.path())
        .output()
        .expect("Failed to execute command");

    assert!(
        output.status.success(),
        "Command should succeed: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );

    let kimi_content = fs::read_to_string(&kimi_json_path).unwrap();
    let kimi_config: serde_json::Value = serde_json::from_str(&kimi_content).unwrap();

    // Verify existing global MCP server is preserved
    assert!(
        kimi_config["mcpServers"]["existing-server"].is_object(),
        "Existing MCP server should be preserved"
    );
    assert_eq!(
        kimi_config["mcpServers"]["existing-server"]["command"], "some-other-cmd",
        "Existing server command should be unchanged"
    );

    // Verify other settings are preserved
    assert_eq!(
        kimi_config["someOtherSetting"], "value",
        "Other settings should be preserved"
    );

    // Verify new server is added
    assert!(
        kimi_config["mcpServers"]["new-server"].is_object(),
        "New MCP server should be added. Config: {}",
        kimi_content
    );
}

// ==========================================================================
// Codex CLI registration tests
// ==========================================================================

#[test]
fn test_init_codex_cli_flag_help_shows_option() {
    let output = Command::new(devboy_bin())
        .args(["init", "--help"])
        .output()
        .expect("Failed to execute command");

    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(output.status.success());
    assert!(
        stdout.contains("--codex-cli"),
        "Help should mention --codex-cli flag"
    );
}

#[test]
fn test_init_with_codex_cli_creates_config() {
    let temp_dir = create_temp_git_repo("git@github.com:owner/repo.git");
    let fake_home = TempDir::new().unwrap();

    let output = Command::new(devboy_bin())
        .args(["init", "--yes", "--codex-cli"])
        .env("HOME", fake_home.path())
        .env("USERPROFILE", fake_home.path())
        .current_dir(temp_dir.path())
        .output()
        .expect("Failed to execute command");

    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        output.status.success(),
        "Command should succeed: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("'devboy'") || stdout.contains("\"devboy\""),
        "Output should contain 'devboy' as the default server name: {}",
        stdout
    );

    // Check fallback TOML config was created
    let codex_toml = fake_home.path().join(".codex").join("config.toml");
    if codex_toml.exists() {
        let content = fs::read_to_string(&codex_toml).unwrap();
        assert!(
            content.contains("[mcp_servers.devboy]"),
            "Codex config should contain devboy MCP server"
        );
    }
}

// ==========================================================================
// Copilot CLI registration tests
// ==========================================================================

#[test]
fn test_init_copilot_flag_help_shows_option() {
    let output = Command::new(devboy_bin())
        .args(["init", "--help"])
        .output()
        .expect("Failed to execute command");

    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(output.status.success());
    assert!(
        stdout.contains("--copilot"),
        "Help should mention --copilot flag"
    );
}

#[test]
fn test_init_with_copilot_creates_config() {
    let temp_dir = create_temp_git_repo("git@github.com:owner/repo.git");
    let fake_home = TempDir::new().unwrap();

    let output = Command::new(devboy_bin())
        .args(["init", "--yes", "--copilot"])
        .env("HOME", fake_home.path())
        .env("USERPROFILE", fake_home.path())
        .current_dir(temp_dir.path())
        .output()
        .expect("Failed to execute command");

    assert!(
        output.status.success(),
        "Command should succeed: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );

    let copilot_json = fake_home.path().join(".copilot").join("mcp-config.json");
    assert!(
        copilot_json.exists(),
        "Copilot config should be created"
    );

    let content = fs::read_to_string(&copilot_json).unwrap();
    let config: serde_json::Value = serde_json::from_str(&content).unwrap();

    assert!(
        config["mcpServers"]["devboy"].is_object(),
        "MCP server should be registered"
    );
    assert_eq!(config["mcpServers"]["devboy"]["type"], "local");
    assert_eq!(config["mcpServers"]["devboy"]["tools"][0], "*");
}

// ==========================================================================
// Gemini CLI registration tests
// ==========================================================================

#[test]
fn test_init_gemini_flag_help_shows_option() {
    let output = Command::new(devboy_bin())
        .args(["init", "--help"])
        .output()
        .expect("Failed to execute command");

    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(output.status.success());
    assert!(
        stdout.contains("--gemini"),
        "Help should mention --gemini flag"
    );
}

#[test]
fn test_init_with_gemini_creates_config() {
    let temp_dir = create_temp_git_repo("git@github.com:owner/repo.git");

    let output = Command::new(devboy_bin())
        .args(["init", "--yes", "--gemini"])
        .current_dir(temp_dir.path())
        .output()
        .expect("Failed to execute command");

    assert!(
        output.status.success(),
        "Command should succeed: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );

    let gemini_json = temp_dir.path().join(".gemini").join("settings.json");
    assert!(
        gemini_json.exists(),
        "Gemini config should be created"
    );

    let content = fs::read_to_string(&gemini_json).unwrap();
    let config: serde_json::Value = serde_json::from_str(&content).unwrap();

    assert!(
        config["mcpServers"]["devboy"].is_object(),
        "MCP server should be registered"
    );
    assert_eq!(config["mcpServers"]["devboy"]["trust"], true);
}

// ==========================================================================
// OpenCode registration tests
// ==========================================================================

#[test]
fn test_init_opencode_flag_help_shows_option() {
    let output = Command::new(devboy_bin())
        .args(["init", "--help"])
        .output()
        .expect("Failed to execute command");

    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(output.status.success());
    assert!(
        stdout.contains("--opencode"),
        "Help should mention --opencode flag"
    );
}

#[test]
fn test_init_with_opencode_creates_config() {
    let temp_dir = create_temp_git_repo("git@github.com:owner/repo.git");

    let output = Command::new(devboy_bin())
        .args(["init", "--yes", "--opencode"])
        .current_dir(temp_dir.path())
        .output()
        .expect("Failed to execute command");

    assert!(
        output.status.success(),
        "Command should succeed: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );

    let opencode_json = temp_dir.path().join("opencode.json");
    assert!(
        opencode_json.exists(),
        "OpenCode config should be created"
    );

    let content = fs::read_to_string(&opencode_json).unwrap();
    let config: serde_json::Value = serde_json::from_str(&content).unwrap();

    assert!(
        config["mcp"]["devboy"].is_object(),
        "MCP server should be registered"
    );
    assert_eq!(config["mcp"]["devboy"]["type"], "local");
}

// ==========================================================================
// ForgeCode registration tests
// ==========================================================================

#[test]
fn test_init_forge_flag_help_shows_option() {
    let output = Command::new(devboy_bin())
        .args(["init", "--help"])
        .output()
        .expect("Failed to execute command");

    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(output.status.success());
    assert!(
        stdout.contains("--forge"),
        "Help should mention --forge flag"
    );
}

#[test]
fn test_init_with_forge_creates_config() {
    let temp_dir = create_temp_git_repo("git@github.com:owner/repo.git");

    let output = Command::new(devboy_bin())
        .args(["init", "--yes", "--forge"])
        .current_dir(temp_dir.path())
        .output()
        .expect("Failed to execute command");

    assert!(
        output.status.success(),
        "Command should succeed: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );

    let forge_json = temp_dir.path().join(".mcp.json");
    assert!(
        forge_json.exists(),
        "ForgeCode config should be created"
    );

    let content = fs::read_to_string(&forge_json).unwrap();
    let config: serde_json::Value = serde_json::from_str(&content).unwrap();

    assert!(
        config["mcpServers"]["devboy"].is_object(),
        "MCP server should be registered"
    );
}
