//! End-to-end acceptance coverage for `devboy otel scan` (#242).
//!
//! These tests intentionally spawn the CLI: the scanner crate's unit tests
//! cover matching behaviour, while this file locks down Clap wiring, input
//! adapters, report formats, and process exit codes.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use rusqlite::Connection;
use tempfile::TempDir;

fn devboy_bin() -> PathBuf {
    let mut path = std::env::current_exe().expect("test executable path");
    path.pop(); // test executable
    path.pop(); // deps/
    path.push(format!("devboy{}", std::env::consts::EXE_SUFFIX));
    path
}

fn command(args: &[&str]) -> Command {
    let mut command = Command::new(devboy_bin());
    command.args(args);
    command
}

fn write_jsonl(dir: &TempDir, name: &str, body: &str) -> PathBuf {
    let path = dir.path().join(name);
    fs::write(&path, body).expect("write JSONL fixture");
    path
}

fn path_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

#[test]
fn help_and_file_outputs_are_available_through_the_cli() {
    let help = command(&["otel", "scan", "--help"])
        .output()
        .expect("run otel scan --help");
    assert!(help.status.success());
    let help_text = String::from_utf8_lossy(&help.stdout);
    assert!(help_text.contains("redacted-jsonl") && help_text.contains("--fail-on"));

    let dir = TempDir::new().expect("temporary directory");
    let input = write_jsonl(&dir, "traces.jsonl", "{\"message\":\"safe\"}\n");
    let input = path_string(&input);

    let json = command(&["otel", "scan", "--input", &input, "--output", "json"])
        .output()
        .expect("run JSON output");
    assert_eq!(json.status.code(), Some(0));
    let report: serde_json::Value =
        serde_json::from_slice(&json.stdout).expect("valid JSON report");
    assert_eq!(report["scan_summary"]["records"], 1);
    assert_eq!(report["findings"], serde_json::json!([]));

    let sarif = command(&["otel", "scan", "--input", &input, "--output", "sarif"])
        .output()
        .expect("run SARIF output");
    assert_eq!(sarif.status.code(), Some(0));
    let sarif: serde_json::Value = serde_json::from_slice(&sarif.stdout).expect("valid SARIF JSON");
    assert_eq!(sarif["version"], "2.1.0");
    assert_eq!(
        sarif["runs"][0]["tool"]["driver"]["name"],
        "devboy otel scan"
    );
}

#[test]
fn directory_sqlite_and_stdin_adapters_report_findings() {
    let dir = TempDir::new().expect("temporary directory");
    let logs = dir.path().join("logs");
    fs::create_dir(&logs).expect("create logs directory");
    fs::write(logs.join("safe.jsonl"), "{\"message\":\"safe\"}\n").expect("write safe log");
    fs::write(
        logs.join("leak.jsonl"),
        "{\"command\":\"Bearer ghp_abcdefghijklmnopqrstuvwxyzABCDEFGHIJ\"}\n",
    )
    .expect("write leaking log");
    let logs = format!("jsonl:{}", logs.display());

    let directory = command(&["otel", "scan", "--input", &logs, "--output", "json"])
        .output()
        .expect("scan JSONL directory");
    assert_eq!(directory.status.code(), Some(1));
    let directory_report: serde_json::Value =
        serde_json::from_slice(&directory.stdout).expect("directory JSON report");
    assert_eq!(directory_report["scan_summary"]["files"], 2);
    assert_eq!(directory_report["scan_summary"]["high"], 1);

    let database = dir.path().join("otel.db");
    let connection = Connection::open(&database).expect("create SQLite fixture");
    connection
        .execute_batch(
            "CREATE TABLE traces (attributes TEXT);\
             INSERT INTO traces VALUES ('{\"token\":\"ghp_abcdefghijklmnopqrstuvwxyzABCDEFGHIJ\"}');",
        )
        .expect("seed SQLite fixture");
    drop(connection);
    let database = format!("sqlite:{}", database.display());
    let sqlite = command(&["otel", "scan", "--input", &database, "--output", "text"])
        .output()
        .expect("scan SQLite database");
    assert_eq!(sqlite.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&sqlite.stdout).contains("github-pat"));

    let mut stdin = command(&[
        "otel",
        "scan",
        "--input",
        "stdin",
        "--output",
        "redacted-jsonl",
    ]);
    stdin.stdin(Stdio::piped());
    stdin.stdout(Stdio::piped());
    let mut child = stdin.spawn().expect("spawn stdin scan");
    child
        .stdin
        .as_mut()
        .expect("stdin pipe")
        .write_all(b"{\"token\":\"ghp_abcdefghijklmnopqrstuvwxyzABCDEFGHIJ\"}\n")
        .expect("write stdin fixture");
    let redacted = child.wait_with_output().expect("read stdin scan result");
    assert_eq!(redacted.status.code(), Some(0));
    let redacted = String::from_utf8(redacted.stdout).expect("redacted output is UTF-8");
    assert!(redacted.contains("[REDACTED:github-pat]") && !redacted.contains("ghp_abc"));
}

#[test]
fn exit_codes_distinguish_low_findings_and_input_errors() {
    let dir = TempDir::new().expect("temporary directory");
    let entropy = "A7f3K9mQ2xV8nL4pR6tY1wC5dH0jB3eG7sU9iO2aS6kF4zX8qW1vN5rT0cP3lD";
    let low = write_jsonl(&dir, "low.jsonl", &format!("{{\"value\":\"{entropy}\"}}\n"));
    let low = path_string(&low);
    let low_result = command(&["otel", "scan", "--input", &low])
        .output()
        .expect("scan low finding");
    assert_eq!(low_result.status.code(), Some(2));

    let malformed = write_jsonl(&dir, "malformed.jsonl", "not valid JSON\n");
    let malformed = path_string(&malformed);
    let error = command(&["otel", "scan", "--input", &malformed])
        .output()
        .expect("scan malformed input");
    assert_eq!(error.status.code(), Some(3));
    assert!(String::from_utf8_lossy(&error.stderr).contains("invalid JSON on line 1"));
}
