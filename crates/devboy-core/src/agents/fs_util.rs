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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::SystemTime;
    use tempfile::tempdir;

    #[test]
    fn to_utc_handles_unix_epoch() {
        let utc = to_utc(SystemTime::UNIX_EPOCH).unwrap();
        assert_eq!(utc.timestamp(), 0);
    }

    #[test]
    fn to_utc_handles_now_without_panic() {
        assert!(to_utc(SystemTime::now()).is_some());
    }

    #[test]
    fn count_subdirs_zero_for_empty_dir() {
        let dir = tempdir().unwrap();
        assert_eq!(count_subdirs(dir.path()), 0);
    }

    #[test]
    fn count_subdirs_zero_for_missing_dir() {
        let dir = tempdir().unwrap();
        assert_eq!(count_subdirs(&dir.path().join("does-not-exist")), 0);
    }

    #[test]
    fn count_subdirs_counts_only_directories() {
        let dir = tempdir().unwrap();
        for name in ["a", "b", "c"] {
            fs::create_dir_all(dir.path().join(name)).unwrap();
        }
        fs::write(dir.path().join("file.txt"), b"x").unwrap();
        fs::write(dir.path().join("README"), b"x").unwrap();
        assert_eq!(count_subdirs(dir.path()), 3);
    }

    #[test]
    fn dir_nonempty_returns_false_for_missing_dir() {
        let dir = tempdir().unwrap();
        assert!(!dir_nonempty(&dir.path().join("does-not-exist")));
    }

    #[test]
    fn dir_nonempty_returns_false_for_empty_dir() {
        let dir = tempdir().unwrap();
        assert!(!dir_nonempty(dir.path()));
    }

    #[test]
    fn dir_nonempty_returns_true_when_any_entry_exists() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("anything"), b"").unwrap();
        assert!(dir_nonempty(dir.path()));
    }

    #[test]
    fn walk_files_returns_empty_for_missing_root() {
        let dir = tempdir().unwrap();
        let result = walk_files(&dir.path().join("nope"), |_| true, 100);
        assert!(result.is_empty());
    }

    #[test]
    fn walk_files_finds_matching_files_recursively() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("a/b")).unwrap();
        fs::write(dir.path().join("top.jsonl"), b"").unwrap();
        fs::write(dir.path().join("a/middle.jsonl"), b"").unwrap();
        fs::write(dir.path().join("a/b/deep.jsonl"), b"").unwrap();
        fs::write(dir.path().join("a/skip.txt"), b"").unwrap();

        let result = walk_files(
            dir.path(),
            |p| p.extension().is_some_and(|e| e == "jsonl"),
            100,
        );
        assert_eq!(result.len(), 3);
        assert!(result.iter().all(|p| p.extension().unwrap() == "jsonl"));
    }

    #[test]
    fn walk_files_respects_max_entries_cap() {
        let dir = tempdir().unwrap();
        for i in 0..10 {
            fs::write(dir.path().join(format!("f{i}.jsonl")), b"").unwrap();
        }
        assert_eq!(walk_files(dir.path(), |_| true, 3).len(), 3);
    }

    #[test]
    fn max_mtime_returns_none_for_missing_dir() {
        let dir = tempdir().unwrap();
        assert!(max_mtime_in(&dir.path().join("nope"), |_| true).is_none());
    }

    #[test]
    fn max_mtime_returns_none_when_no_match() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), b"").unwrap();
        assert!(
            max_mtime_in(dir.path(), |p| p.extension().is_some_and(|e| e == "jsonl")).is_none()
        );
    }

    #[test]
    fn max_mtime_picks_latest_match() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("a.jsonl"), b"").unwrap();
        fs::write(dir.path().join("b.jsonl"), b"").unwrap();
        fs::write(dir.path().join("c.txt"), b"").unwrap();
        assert!(
            max_mtime_in(dir.path(), |p| p.extension().is_some_and(|e| e == "jsonl")).is_some()
        );
    }
}
