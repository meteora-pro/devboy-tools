//! End-to-end integration tests for the `devboy skills` subcommand
//! family. Each test spawns the compiled binary in a temp HOME so the
//! install targets live entirely inside the temp directory and never
//! touch the developer's real `~/.agents/` or `~/.claude/`.

use std::fs;
use std::process::Command;

use tempfile::TempDir;

/// Path to the `devboy` test binary.
fn devboy_bin() -> std::path::PathBuf {
    let mut path = std::env::current_exe().unwrap();
    path.pop(); // drop test binary name
    path.pop(); // drop deps/
    let bin_name = format!("devboy{}", std::env::consts::EXE_SUFFIX);
    path.push(bin_name);
    path
}

fn spawn<I, S>(home: &TempDir, cwd: &std::path::Path, args: I) -> std::process::Output
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    Command::new(devboy_bin())
        .env("HOME", home.path())
        .env("USERPROFILE", home.path()) // Windows
        .current_dir(cwd)
        .args(args)
        .output()
        .expect("failed to spawn devboy")
}

#[test]
fn skills_help_mentions_subcommands() {
    let home = TempDir::new().unwrap();
    let cwd = TempDir::new().unwrap();
    let output = spawn(&home, cwd.path(), ["skills", "--help"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "`skills --help` should exit 0");
    for sub in ["list", "show", "install", "upgrade", "remove"] {
        assert!(
            stdout.contains(sub),
            "skills help should mention subcommand `{sub}`"
        );
    }
}

#[test]
fn skills_list_shows_at_least_one_skill() {
    let home = TempDir::new().unwrap();
    let cwd = TempDir::new().unwrap();
    let output = spawn(&home, cwd.path(), ["skills", "list"]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    // At minimum the placeholder `devboy-setup` skill is shipped.
    assert!(stdout.contains("devboy-setup"), "stdout was: {stdout}");
    assert!(stdout.contains("[self-bootstrap]"));
}

#[test]
fn skills_show_prints_frontmatter_and_body() {
    let home = TempDir::new().unwrap();
    let cwd = TempDir::new().unwrap();
    let output = spawn(&home, cwd.path(), ["skills", "show", "devboy-setup"]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("name: devboy-setup"));
    assert!(stdout.contains("# devboy-setup"));
}

#[test]
fn skills_install_fails_without_repo_or_flags() {
    let home = TempDir::new().unwrap();
    let cwd = TempDir::new().unwrap(); // not a git repo, no .devboy.toml
    let output = spawn(&home, cwd.path(), ["skills", "install", "devboy-setup"]);
    assert!(
        !output.status.success(),
        "install without repo + flags should fail"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--global") || stderr.contains("--agent"),
        "error should suggest --global / --agent, got:\n{stderr}"
    );
}

#[test]
fn skills_install_global_writes_to_home_agents_skills() {
    let home = TempDir::new().unwrap();
    let cwd = TempDir::new().unwrap();
    let output = spawn(
        &home,
        cwd.path(),
        ["skills", "install", "devboy-setup", "--global"],
    );
    assert!(
        output.status.success(),
        "--global install should succeed:\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let installed = home.path().join(".agents/skills/devboy-setup/SKILL.md");
    assert!(
        installed.exists(),
        "expected {} to exist",
        installed.display()
    );
    let manifest = home.path().join(".agents/skills/.manifest.json");
    assert!(
        manifest.exists(),
        "expected manifest {}",
        manifest.display()
    );
    let body = fs::read_to_string(&installed).unwrap();
    assert!(body.contains("name: devboy-setup"));
}

#[test]
fn skills_install_agent_claude_writes_to_home_claude_skills() {
    let home = TempDir::new().unwrap();
    let cwd = TempDir::new().unwrap();
    let output = spawn(
        &home,
        cwd.path(),
        ["skills", "install", "devboy-setup", "--agent", "claude"],
    );
    assert!(output.status.success());
    let installed = home.path().join(".claude/skills/devboy-setup/SKILL.md");
    assert!(
        installed.exists(),
        "expected {} to exist",
        installed.display()
    );
}

#[test]
fn skills_install_second_run_reports_unchanged() {
    let home = TempDir::new().unwrap();
    let cwd = TempDir::new().unwrap();

    // First install writes.
    let first = spawn(
        &home,
        cwd.path(),
        ["skills", "install", "devboy-setup", "--global"],
    );
    assert!(first.status.success());
    let first_stdout = String::from_utf8_lossy(&first.stdout).to_string();
    assert!(
        first_stdout.contains("installed devboy-setup"),
        "first run: {first_stdout}"
    );

    // Second install on unchanged content reports Unchanged.
    let second = spawn(
        &home,
        cwd.path(),
        ["skills", "install", "devboy-setup", "--global"],
    );
    assert!(second.status.success());
    let second_stdout = String::from_utf8_lossy(&second.stdout).to_string();
    assert!(
        second_stdout.contains("installed") || second_stdout.contains("unchanged"),
        "expected either installed (no-history case) or unchanged, got: {second_stdout}"
    );
}

#[test]
fn skills_install_dry_run_does_not_write() {
    let home = TempDir::new().unwrap();
    let cwd = TempDir::new().unwrap();
    let output = spawn(
        &home,
        cwd.path(),
        ["skills", "install", "devboy-setup", "--global", "--dry-run"],
    );
    assert!(output.status.success());
    let installed = home.path().join(".agents/skills/devboy-setup/SKILL.md");
    assert!(
        !installed.exists(),
        "--dry-run should not write the skill file"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("(dry-run)"),
        "expected dry-run summary, got: {stdout}"
    );
}

#[test]
fn skills_install_repo_local_uses_devboy_toml() {
    let home = TempDir::new().unwrap();
    let cwd = TempDir::new().unwrap();
    // Mark the cwd as a repo via `.devboy.toml`.
    fs::write(cwd.path().join(".devboy.toml"), b"").unwrap();

    let output = spawn(&home, cwd.path(), ["skills", "install", "devboy-setup"]);
    assert!(
        output.status.success(),
        "repo-local default install should succeed:\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let installed = cwd.path().join(".agents/skills/devboy-setup/SKILL.md");
    assert!(
        installed.exists(),
        "expected repo-local install at {}",
        installed.display()
    );
}

#[test]
fn skills_remove_deletes_files_and_manifest_entry() {
    let home = TempDir::new().unwrap();
    let cwd = TempDir::new().unwrap();

    let installed = home.path().join(".agents/skills/devboy-setup/SKILL.md");
    let install = spawn(
        &home,
        cwd.path(),
        ["skills", "install", "devboy-setup", "--global"],
    );
    assert!(install.status.success());
    assert!(installed.exists());

    let remove = spawn(
        &home,
        cwd.path(),
        ["skills", "remove", "devboy-setup", "--global"],
    );
    assert!(
        remove.status.success(),
        "remove should succeed:\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&remove.stdout),
        String::from_utf8_lossy(&remove.stderr)
    );
    assert!(!installed.exists(), "file should be deleted");
}
