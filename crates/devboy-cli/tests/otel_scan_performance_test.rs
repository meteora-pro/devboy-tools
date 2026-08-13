//! Opt-in performance acceptance test for issue #242.
//!
//! Run with:
//! `cargo test -p devboy-cli --test otel_scan_performance_test -- --ignored`

use std::fs::File;
use std::io::{BufWriter, Seek, Write};
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use tempfile::TempDir;

const ARTIFACT_BYTES: u64 = 100 * 1024 * 1024;
const MAX_SCAN_TIME: Duration = Duration::from_secs(30);

fn devboy_bin() -> PathBuf {
    let mut path = std::env::current_exe().expect("test executable path");
    path.pop(); // test executable
    path.pop(); // deps/
    path.push(format!("devboy{}", std::env::consts::EXE_SUFFIX));
    path
}

/// Generates the fixture incrementally so the benchmark does not pre-load its
/// 100 MiB input into memory. A single JSONL file deliberately exercises the
/// scanner's single-threaded file path, not Rayon directory parallelism.
fn write_100_mib_jsonl(dir: &TempDir) -> PathBuf {
    let path = dir.path().join("otel-100-mib.jsonl");
    let file = File::create(&path).expect("create performance fixture");
    let mut writer = BufWriter::new(file);
    let record = format!(
        "{{\"span_id\":\"perf\",\"message\":\"{}\"}}\n",
        "safe telemetry value ".repeat(24)
    );
    while writer.stream_position().expect("fixture position") < ARTIFACT_BYTES {
        writer
            .write_all(record.as_bytes())
            .expect("write performance fixture record");
    }
    writer.flush().expect("flush performance fixture");
    assert!(
        std::fs::metadata(&path).expect("fixture metadata").len() >= ARTIFACT_BYTES,
        "fixture must be at least 100 MiB"
    );
    path
}

#[test]
#[ignore = "performance acceptance test; run explicitly on a representative CI runner"]
fn scans_100_mib_jsonl_in_under_30_seconds_on_one_file() {
    let dir = TempDir::new().expect("temporary directory");
    let input = write_100_mib_jsonl(&dir);
    let started = Instant::now();
    let output = Command::new(devboy_bin())
        .args(["otel", "scan", "--input"])
        .arg(&input)
        .args(["--output", "json", "--fail-on", "none"])
        .output()
        .expect("spawn devboy otel scan");
    let elapsed = started.elapsed();

    assert!(
        output.status.success(),
        "scan failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        elapsed < MAX_SCAN_TIME,
        "100 MiB scan took {elapsed:.2?}; limit is {MAX_SCAN_TIME:.2?}"
    );
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("scan output is JSON");
    assert_eq!(report["scan_summary"]["findings_total"], 0);
}
