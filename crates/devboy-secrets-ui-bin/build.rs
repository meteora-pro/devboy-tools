//! Build script for `devboy-secrets-ui-bin` — emits compile-time
//! environment variables consumed by the K23 About modal:
//!
//! * `DEVBOY_GIT_HASH`        — short git SHA + optional `-dirty`
//! * `DEVBOY_BUILD_TIMESTAMP` — RFC 3339 UTC of the build start
//! * `DEVBOY_RUSTC_VERSION`   — full `rustc -V` line
//! * `DEVBOY_TARGET_TRIPLE`   — cargo-supplied target triple
//!
//! Falls back to `"unknown"` for any value that fails to resolve
//! (clean tarball builds, sandboxed CI without git, etc.) so the
//! About modal never breaks the build.
//!
//! Re-runs whenever HEAD moves or the source tree changes so the
//! reported SHA stays accurate during local development.

use std::process::Command;

fn main() {
    // Re-run on every git HEAD move + index update so cached
    // builds don't pin an outdated SHA. The `.git/HEAD` watch
    // catches branch switches; `.git/index` catches stage-/
    // unstage cycles that flip the `-dirty` suffix.
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/index");
    // Also re-run if env-vars we read change. Belt + braces:
    // CI usually injects these.
    println!("cargo:rerun-if-env-changed=DEVBOY_GIT_HASH_OVERRIDE");

    let git_hash = git_hash_with_dirty().unwrap_or_else(|| "unknown".to_owned());
    let build_ts = chrono_like_utc_now();
    let rustc_ver = run_capture(&["rustc", "-V"]).unwrap_or_else(|| "unknown".to_owned());
    let target = std::env::var("TARGET").unwrap_or_else(|_| "unknown".to_owned());

    println!("cargo:rustc-env=DEVBOY_GIT_HASH={git_hash}");
    println!("cargo:rustc-env=DEVBOY_BUILD_TIMESTAMP={build_ts}");
    println!("cargo:rustc-env=DEVBOY_RUSTC_VERSION={rustc_ver}");
    println!("cargo:rustc-env=DEVBOY_TARGET_TRIPLE={target}");
}

/// Short SHA + `-dirty` suffix when the working tree has
/// uncommitted changes. Returns `None` when `git` isn't on PATH
/// or the build runs outside a repo (tarball, vendored crate).
fn git_hash_with_dirty() -> Option<String> {
    // Allow CI to inject a verbatim string (e.g. GitLab pipeline
    // already knows the commit and wants to skip the git shell-
    // out). Empty values fall through.
    if let Ok(v) = std::env::var("DEVBOY_GIT_HASH_OVERRIDE")
        && !v.is_empty()
    {
        return Some(v);
    }
    let sha = run_capture(&["git", "rev-parse", "--short=10", "HEAD"])?;
    let dirty = run_capture(&["git", "status", "--porcelain"])
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);
    Some(if dirty { format!("{sha}-dirty") } else { sha })
}

/// Minimal stdout capture: returns trimmed stdout on success,
/// `None` on any non-zero exit or spawn failure.
fn run_capture(argv: &[&str]) -> Option<String> {
    let out = Command::new(argv[0]).args(&argv[1..]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?;
    Some(s.trim().to_owned())
}

/// Build-time timestamp in RFC 3339 UTC, formatted by hand to
/// avoid a build-dep on chrono — keeps the build-script crate
/// graph empty.
fn chrono_like_utc_now() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format_rfc3339_utc(secs)
}

/// `secs` since UNIX epoch → `YYYY-MM-DDTHH:MM:SSZ`. Manual
/// Gregorian calendar math; good enough for a build-info string,
/// not for anything load-bearing.
fn format_rfc3339_utc(secs: u64) -> String {
    let mut s = secs;
    let secs_in_day = 86_400u64;
    let days = s / secs_in_day;
    s %= secs_in_day;
    let hours = s / 3600;
    s %= 3600;
    let mins = s / 60;
    let secs_rem = s % 60;

    // 1970-01-01 = day 0
    let (year, month, day) = days_to_ymd(days);
    format!("{year:04}-{month:02}-{day:02}T{hours:02}:{mins:02}:{secs_rem:02}Z")
}

/// Convert days-since-epoch into `(year, month, day)`. Standard
/// Gregorian algorithm (Howard Hinnant / civil_from_days).
fn days_to_ymd(days: u64) -> (i32, u32, u32) {
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y as i32, m as u32, d as u32)
}
