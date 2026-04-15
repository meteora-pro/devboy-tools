//! Build script that embeds build metadata into the binary.
//!
//! Available at runtime via `env!()` macros:
//! - `DEVBOY_BUILD_COMMIT` — git commit SHA (short)
//! - `DEVBOY_BUILD_COMMIT_FULL` — git commit SHA (full)
//! - `DEVBOY_BUILD_TIMESTAMP` — ISO 8601 build time
//! - `DEVBOY_BUILD_CI_RUN_ID` — CI pipeline/run ID (empty if local build)
//! - `DEVBOY_BUILD_DIRTY` — "true" if working tree had uncommitted changes

use std::process::Command;

fn main() {
    // Git commit SHA (short)
    let commit_short = git(&["rev-parse", "--short", "HEAD"]);
    println!("cargo:rustc-env=DEVBOY_BUILD_COMMIT={commit_short}");

    // Git commit SHA (full)
    let commit_full = git(&["rev-parse", "HEAD"]);
    println!("cargo:rustc-env=DEVBOY_BUILD_COMMIT_FULL={commit_full}");

    // Dirty flag
    let dirty = !git(&["status", "--porcelain"]).is_empty();
    println!("cargo:rustc-env=DEVBOY_BUILD_DIRTY={dirty}");

    // Build timestamp (ISO 8601)
    let timestamp = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    println!("cargo:rustc-env=DEVBOY_BUILD_TIMESTAMP={timestamp}");

    // CI run ID (GitHub Actions: GITHUB_RUN_ID, GitLab: CI_PIPELINE_ID)
    let ci_run_id = std::env::var("GITHUB_RUN_ID")
        .or_else(|_| std::env::var("CI_PIPELINE_ID"))
        .unwrap_or_default();
    println!("cargo:rustc-env=DEVBOY_BUILD_CI_RUN_ID={ci_run_id}");

    // Rerun if git state changes
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/refs/");
    println!("cargo:rerun-if-changed=../../.git/packed-refs");
    println!("cargo:rerun-if-changed=../../.git/index");
}

fn git(args: &[&str]) -> String {
    Command::new("git")
        .args(args)
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                String::from_utf8(o.stdout).ok()
            } else {
                None
            }
        })
        .unwrap_or_default()
        .trim()
        .to_string()
}
