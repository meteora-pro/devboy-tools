//! Shared filesystem helpers used by per-agent detectors.
//!
//! Walking conventions:
//! - All helpers take an explicit root and never look at process cwd.
//! - Errors surface as `None` (silent) — detectors must not panic on a
//!   missing or unreadable path; that's the natural "agent not installed"
//!   outcome.

use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use std::time::SystemTime;

/// Convert a SystemTime to a UTC datetime.
pub(super) fn to_utc(t: SystemTime) -> Option<DateTime<Utc>> {
    let dur = t.duration_since(SystemTime::UNIX_EPOCH).ok()?;
    DateTime::<Utc>::from_timestamp(dur.as_secs() as i64, dur.subsec_nanos())
}

/// Max mtime across the directory entries matching `predicate`. Walks one
/// level deep only; for deeper walks compose with `walk_files`.
pub(super) fn max_mtime_in<P>(root: &Path, predicate: P) -> Option<DateTime<Utc>>
where
    P: Fn(&Path) -> bool,
{
    let entries = fs::read_dir(root).ok()?;
    let mut best: Option<DateTime<Utc>> = None;
    for entry in entries.flatten() {
        let path = entry.path();
        if !predicate(&path) {
            continue;
        }
        if let Ok(meta) = entry.metadata()
            && let Ok(modified) = meta.modified()
            && let Some(t) = to_utc(modified)
        {
            best = Some(best.map_or(t, |b| b.max(t)));
        }
    }
    best
}

/// Walk `root` recursively, collecting paths for which `predicate` is true.
/// Caps at `max_entries` to prevent runaway scans on weirdly-large dirs.
pub(super) fn walk_files<P>(root: &Path, predicate: P, max_entries: usize) -> Vec<PathBuf>
where
    P: Fn(&Path) -> bool + Copy,
{
    let mut out = Vec::new();
    if !root.is_dir() {
        return out;
    }
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if out.len() >= max_entries {
            break;
        }
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                stack.push(path);
            } else if predicate(&path) {
                out.push(path);
                if out.len() >= max_entries {
                    break;
                }
            }
        }
    }
    out
}

/// Count direct subdirectories of `root`.
pub(super) fn count_subdirs(root: &Path) -> u64 {
    let Ok(entries) = fs::read_dir(root) else {
        return 0;
    };
    entries
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .count() as u64
}

/// Whether a directory exists and is non-empty.
pub(super) fn dir_nonempty(p: &Path) -> bool {
    fs::read_dir(p)
        .map(|mut it| it.next().is_some())
        .unwrap_or(false)
}
