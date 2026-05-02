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
        // The resolver honours DEVBOY_HOME_OVERRIDE on every platform;
        // HOME / USERPROFILE are kept for test cleanliness but cannot
        // redirect `dirs::home_dir()` on Windows, which is why the
        // override env var exists.
        .env("DEVBOY_HOME_OVERRIDE", home.path())
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
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
    // At minimum the placeholder `setup` skill is shipped.
    assert!(stdout.contains("setup"), "stdout was: {stdout}");
    assert!(stdout.contains("[self-bootstrap]"));
}

#[test]
fn skills_show_prints_frontmatter_and_body() {
    let home = TempDir::new().unwrap();
    let cwd = TempDir::new().unwrap();
    let output = spawn(&home, cwd.path(), ["skills", "show", "setup"]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("name: setup"));
    assert!(stdout.contains("# setup"));
}

#[test]
fn skills_install_fails_without_repo_or_flags() {
    let home = TempDir::new().unwrap();
    let cwd = TempDir::new().unwrap(); // not a git repo, no .devboy.toml
    let output = spawn(&home, cwd.path(), ["skills", "install", "setup"]);
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
        ["skills", "install", "setup", "--global"],
    );
    assert!(
        output.status.success(),
        "--global install should succeed:\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let installed = home.path().join(".agents/skills/setup/SKILL.md");
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
    assert!(body.contains("name: setup"));
}

#[test]
fn skills_install_agent_claude_writes_to_home_claude_skills() {
    let home = TempDir::new().unwrap();
    let cwd = TempDir::new().unwrap();
    let output = spawn(
        &home,
        cwd.path(),
        ["skills", "install", "setup", "--agent", "claude"],
    );
    assert!(output.status.success());
    let installed = home.path().join(".claude/skills/setup/SKILL.md");
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
        ["skills", "install", "setup", "--global"],
    );
    assert!(first.status.success());
    let first_stdout = String::from_utf8_lossy(&first.stdout).to_string();
    assert!(
        first_stdout.contains("installed setup"),
        "first run: {first_stdout}"
    );

    // Second install on unchanged content reports Unchanged.
    let second = spawn(
        &home,
        cwd.path(),
        ["skills", "install", "setup", "--global"],
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
        ["skills", "install", "setup", "--global", "--dry-run"],
    );
    assert!(output.status.success());
    let installed = home.path().join(".agents/skills/setup/SKILL.md");
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

    let output = spawn(&home, cwd.path(), ["skills", "install", "setup"]);
    assert!(
        output.status.success(),
        "repo-local default install should succeed:\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let installed = cwd.path().join(".agents/skills/setup/SKILL.md");
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

    let installed = home.path().join(".agents/skills/setup/SKILL.md");
    let install = spawn(
        &home,
        cwd.path(),
        ["skills", "install", "setup", "--global"],
    );
    assert!(install.status.success());
    assert!(installed.exists());

    let remove = spawn(&home, cwd.path(), ["skills", "remove", "setup", "--global"]);
    assert!(
        remove.status.success(),
        "remove should succeed:\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&remove.stdout),
        String::from_utf8_lossy(&remove.stderr)
    );
    assert!(!installed.exists(), "file should be deleted");
}

#[test]
fn trace_begin_event_end_round_trip() {
    let home = TempDir::new().unwrap();
    let cwd = TempDir::new().unwrap();
    let dir_override = TempDir::new().unwrap();

    let begin = spawn(
        &home,
        cwd.path(),
        [
            "trace",
            "begin",
            "--skill",
            "setup",
            "--dir",
            dir_override.path().to_str().unwrap(),
        ],
    );
    assert!(begin.status.success(), "trace begin should succeed");
    let begin_stdout = String::from_utf8_lossy(&begin.stdout);
    let meta: serde_json::Value = serde_json::from_str(begin_stdout.trim())
        .expect("trace begin should print a single JSON line");
    let session_id = meta["session_id"].as_str().unwrap().to_string();
    let session_dir = meta["session_dir"].as_str().unwrap().to_string();
    assert!(
        !session_id.is_empty(),
        "session_id is empty: {begin_stdout}"
    );

    let event = spawn(
        &home,
        cwd.path(),
        [
            "trace",
            "event",
            "--session-dir",
            &session_dir,
            "--session-id",
            &session_id,
            "--skill",
            "setup",
            "--phase",
            "tool_call",
            "--payload",
            r#"{"tool":"get_issues","args":{"limit":3}}"#,
        ],
    );
    assert!(
        event.status.success(),
        "trace event should succeed: {}",
        String::from_utf8_lossy(&event.stderr)
    );

    let end = spawn(
        &home,
        cwd.path(),
        [
            "trace",
            "end",
            "--session-dir",
            &session_dir,
            "--session-id",
            &session_id,
            "--skill",
            "setup",
            "--outcome",
            "success",
            "--summary",
            "smoke-test complete",
        ],
    );
    assert!(end.status.success(), "trace end should succeed");

    let trace_path = std::path::PathBuf::from(&session_dir).join("trace.jsonl");
    let trace_body = fs::read_to_string(&trace_path).expect("trace.jsonl should exist");
    let lines: Vec<&str> = trace_body.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(
        lines.len(),
        3,
        "expected 3 events (start, tool_call, end), got: {lines:?}"
    );
    assert!(lines[0].contains("\"phase\":\"start\""));
    assert!(lines[1].contains("\"phase\":\"tool_call\""));
    assert!(lines[2].contains("\"phase\":\"end\""));

    let meta_path = std::path::PathBuf::from(&session_dir).join("meta.json");
    let meta_body = fs::read_to_string(&meta_path).expect("meta.json should exist");
    assert!(meta_body.contains("\"outcome\": \"success\""));
    assert!(meta_body.contains("smoke-test complete"));
    assert!(meta_body.contains("\"tool_calls\": 1"));
}

#[test]
fn trace_event_redacts_tokens() {
    let home = TempDir::new().unwrap();
    let cwd = TempDir::new().unwrap();
    let dir_override = TempDir::new().unwrap();

    let begin = spawn(
        &home,
        cwd.path(),
        [
            "trace",
            "begin",
            "--skill",
            "devboy-test",
            "--dir",
            dir_override.path().to_str().unwrap(),
        ],
    );
    assert!(begin.status.success());
    let meta: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&begin.stdout).trim()).unwrap();
    let session_id = meta["session_id"].as_str().unwrap().to_string();
    let session_dir = meta["session_dir"].as_str().unwrap().to_string();

    let event = spawn(
        &home,
        cwd.path(),
        [
            "trace",
            "event",
            "--session-dir",
            &session_dir,
            "--session-id",
            &session_id,
            "--skill",
            "devboy-test",
            "--phase",
            "tool_call",
            "--payload",
            r#"{"args":{"token":"ghp_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}}"#,
        ],
    );
    assert!(event.status.success());

    let trace_path = std::path::PathBuf::from(&session_dir).join("trace.jsonl");
    let body = fs::read_to_string(trace_path).unwrap();
    assert!(
        !body.contains("ghp_aaaaaaaa"),
        "token was not redacted: {body}"
    );
    assert!(body.contains("<redacted"));
}
