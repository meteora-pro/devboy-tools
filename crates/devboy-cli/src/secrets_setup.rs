//! Repository env-var scanner for the `setup-secrets` skill (P26).
//!
//! Walks a project tree and pulls candidate environment-variable
//! names out of source files, dotenv-style configs, and a
//! handful of project manifests. The wizard turns those
//! candidates into ADR-020 path proposals (P26.3) and feeds
//! them into the provision flow (P26.4).
//!
//! ## What counts as a "candidate"
//!
//! An env-var reference observed in code or in a `.env*` key
//! line. The scanner does NOT verify that the reference is
//! reachable at runtime; that is the wizard's job. Examples
//! that match:
//!
//! - JS / TS: `process.env.JIRA_TOKEN`, `process.env["JIRA_TOKEN"]`,
//!   `process.env['X']`.
//! - Python: `os.getenv("X")`, `os.environ["X"]`,
//!   `os.environ.get("X")`.
//! - Rust: `std::env::var("X")`, `env::var("X")`,
//!   `std::env::var_os("X")`.
//! - Shell-style dotenv (`.env`, `.env.example`, `.env.*`):
//!   bare key lines like `X=value` (case-insensitive `X`,
//!   though the canonical convention is upper-case).
//!
//! Deliberately conservative — false negatives are tolerable
//! (the user can always add a path manually); false positives
//! (e.g. matching a comment) are noisy and waste the user's
//! review time.
//!
//! ## What is skipped
//!
//! Top-level `target`, `node_modules`, `dist`, `build`,
//! `.venv`, `.git`, `.next`, `.devboy/.cache`, plus any path
//! whose name starts with `.` (hidden directories) — symlinks
//! are not followed.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use regex::Regex;

/// One observation of an env-var reference in the project tree.
/// `line` is 1-indexed so it lines up with editor / `grep -n`
/// output.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EnvVarHit {
    pub var_name: String,
    pub file: PathBuf,
    pub line: u32,
}

/// Walk `root` and return every distinct env-var hit. The
/// result is sorted (var, file, line) so callers get
/// deterministic output without an additional sort pass.
pub fn scan_repo(root: &Path) -> std::io::Result<Vec<EnvVarHit>> {
    let patterns = scan_patterns();
    let mut hits: BTreeSet<EnvVarHit> = BTreeSet::new();
    walk_dir(root, &patterns, &mut hits)?;
    Ok(hits.into_iter().collect())
}

// =============================================================================
// Walker
// =============================================================================

const SKIP_DIRS: &[&str] = &[
    "target",
    "node_modules",
    "dist",
    "build",
    ".git",
    ".venv",
    "venv",
    ".next",
    ".turbo",
    ".cache",
    ".idea",
    ".vscode",
];

fn walk_dir(
    dir: &Path,
    patterns: &ScanPatterns,
    hits: &mut BTreeSet<EnvVarHit>,
) -> std::io::Result<()> {
    let entries = match fs::read_dir(dir) {
        Ok(it) => it,
        // Permission errors on a sub-tree should not abort the
        // scan — log nothing, just skip.
        Err(_) => return Ok(()),
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let file_type = match entry.file_type() {
            Ok(t) => t,
            Err(_) => continue,
        };
        // Don't follow symlinks — they can loop.
        if file_type.is_symlink() {
            continue;
        }
        let name = match entry.file_name().to_str() {
            Some(s) => s.to_owned(),
            None => continue,
        };
        if file_type.is_dir() {
            // Skip well-known build / dependency / hidden trees.
            if SKIP_DIRS.contains(&name.as_str()) {
                continue;
            }
            // Hidden dirs (starting with `.`) are skipped except
            // for `.devboy` itself, which may carry a manifest
            // we want to honour later. We don't *need* to scan
            // it for env vars though, so skip too.
            if name.starts_with('.') {
                continue;
            }
            walk_dir(&path, patterns, hits)?;
            continue;
        }
        if file_type.is_file()
            && let Some(scanner) = file_scanner(&path)
            && let Ok(content) = fs::read_to_string(&path)
        {
            scanner(&path, &content, patterns, hits);
        }
    }
    Ok(())
}

// =============================================================================
// File-type dispatch
// =============================================================================

type FileScanner = fn(&Path, &str, &ScanPatterns, &mut BTreeSet<EnvVarHit>);

fn file_scanner(path: &Path) -> Option<FileScanner> {
    let name = path.file_name().and_then(|n| n.to_str())?;
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");

    // Dotenv files: `.env`, `.env.example`, `.env.local`, etc.
    if name == ".env" || name.starts_with(".env.") {
        return Some(scan_dotenv);
    }

    // Project manifests we look at by exact name.
    if matches!(name, "Cargo.toml" | "pyproject.toml" | "package.json") {
        return Some(scan_manifest);
    }

    match ext {
        "rs" | "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" | "py" => Some(scan_source),
        _ => None,
    }
}

// =============================================================================
// Patterns
// =============================================================================

struct ScanPatterns {
    /// Source-code patterns. The first capture group is the
    /// var name.
    source: Vec<Regex>,
    /// Dotenv `KEY=value` matcher; capture 1 is the key.
    dotenv: Regex,
    /// Manifest patterns matching `${VAR}` or `$VAR` references
    /// inside JSON / TOML script blocks.
    manifest: Vec<Regex>,
}

fn scan_patterns() -> ScanPatterns {
    let var = r"([A-Z_][A-Z0-9_]*)";
    let source = vec![
        // process.env.X
        Regex::new(&format!(r"process\.env\.{var}")).unwrap(),
        // process.env["X"] / process.env['X']
        Regex::new(&format!(r#"process\.env\[\s*['"]{var}['"]\s*\]"#)).unwrap(),
        // os.getenv("X") / os.getenv('X')
        Regex::new(&format!(r#"os\.getenv\(\s*['"]{var}['"]"#)).unwrap(),
        // os.environ["X"] / os.environ['X']
        Regex::new(&format!(r#"os\.environ\[\s*['"]{var}['"]\s*\]"#)).unwrap(),
        // os.environ.get("X")
        Regex::new(&format!(r#"os\.environ\.get\(\s*['"]{var}['"]"#)).unwrap(),
        // Rust: std::env::var("X") / env::var("X") /
        // std::env::var_os("X").
        Regex::new(&format!(
            r#"(?:std::)?env::(?:var|var_os|var_optional)\(\s*['"]{var}['"]"#
        ))
        .unwrap(),
    ];
    let dotenv = Regex::new(r"^\s*([A-Z_][A-Z0-9_]*)\s*=").unwrap();
    let manifest = vec![
        // ${VAR} expansions inside JSON / TOML strings.
        Regex::new(&format!(r"\$\{{{var}\}}")).unwrap(),
    ];
    ScanPatterns {
        source,
        dotenv,
        manifest,
    }
}

// =============================================================================
// Per-format scanners
// =============================================================================

fn scan_source(
    path: &Path,
    content: &str,
    patterns: &ScanPatterns,
    hits: &mut BTreeSet<EnvVarHit>,
) {
    for (idx, line) in content.lines().enumerate() {
        if line.trim_start().starts_with("//") || line.trim_start().starts_with('#') {
            continue;
        }
        for re in &patterns.source {
            for cap in re.captures_iter(line) {
                if let Some(m) = cap.get(1) {
                    hits.insert(EnvVarHit {
                        var_name: m.as_str().to_owned(),
                        file: path.to_owned(),
                        line: (idx as u32) + 1,
                    });
                }
            }
        }
    }
}

fn scan_dotenv(
    path: &Path,
    content: &str,
    patterns: &ScanPatterns,
    hits: &mut BTreeSet<EnvVarHit>,
) {
    for (idx, line) in content.lines().enumerate() {
        let trimmed = line.trim_start();
        if trimmed.starts_with('#') || trimmed.is_empty() {
            continue;
        }
        if let Some(cap) = patterns.dotenv.captures(trimmed)
            && let Some(m) = cap.get(1)
        {
            hits.insert(EnvVarHit {
                var_name: m.as_str().to_owned(),
                file: path.to_owned(),
                line: (idx as u32) + 1,
            });
        }
    }
}

fn scan_manifest(
    path: &Path,
    content: &str,
    patterns: &ScanPatterns,
    hits: &mut BTreeSet<EnvVarHit>,
) {
    for (idx, line) in content.lines().enumerate() {
        for re in &patterns.manifest {
            for cap in re.captures_iter(line) {
                if let Some(m) = cap.get(1) {
                    hits.insert(EnvVarHit {
                        var_name: m.as_str().to_owned(),
                        file: path.to_owned(),
                        line: (idx as u32) + 1,
                    });
                }
            }
        }
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write(dir: &Path, rel: &str, body: &str) {
        let full = dir.join(rel);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(full, body).unwrap();
    }

    fn names(hits: &[EnvVarHit]) -> Vec<&str> {
        hits.iter().map(|h| h.var_name.as_str()).collect()
    }

    #[test]
    fn scans_typescript_process_env_dot_form() {
        let dir = TempDir::new().unwrap();
        write(
            dir.path(),
            "src/index.ts",
            "const t = process.env.JIRA_TOKEN;\nconst u = process.env.SLACK_BOT;\n",
        );
        let hits = scan_repo(dir.path()).unwrap();
        assert!(names(&hits).contains(&"JIRA_TOKEN"));
        assert!(names(&hits).contains(&"SLACK_BOT"));
    }

    #[test]
    fn scans_typescript_bracket_form() {
        let dir = TempDir::new().unwrap();
        write(
            dir.path(),
            "src/cfg.ts",
            r#"const t = process.env["DATABASE_URL"];"#,
        );
        let hits = scan_repo(dir.path()).unwrap();
        assert_eq!(names(&hits), vec!["DATABASE_URL"]);
    }

    #[test]
    fn scans_python_three_forms() {
        let dir = TempDir::new().unwrap();
        write(
            dir.path(),
            "app.py",
            r#"
import os
a = os.getenv("OPENAI_KEY")
b = os.environ["GITHUB_TOKEN"]
c = os.environ.get("SLACK_BOT")
"#,
        );
        let hits = scan_repo(dir.path()).unwrap();
        let mut got = names(&hits);
        got.sort();
        assert_eq!(got, vec!["GITHUB_TOKEN", "OPENAI_KEY", "SLACK_BOT"]);
    }

    #[test]
    fn scans_rust_std_env_var() {
        let dir = TempDir::new().unwrap();
        write(
            dir.path(),
            "src/main.rs",
            r#"
fn main() {
    let _t = std::env::var("MY_TOKEN").unwrap();
    let _u = env::var("OTHER").ok();
    let _v = std::env::var_os("YET_ANOTHER");
}
"#,
        );
        let hits = scan_repo(dir.path()).unwrap();
        let mut got = names(&hits);
        got.sort();
        assert_eq!(got, vec!["MY_TOKEN", "OTHER", "YET_ANOTHER"]);
    }

    #[test]
    fn scans_dotenv_keys() {
        let dir = TempDir::new().unwrap();
        write(
            dir.path(),
            ".env",
            "# comment\n\nDATABASE_URL=postgres://...\nAPI_KEY=xyz\n",
        );
        write(dir.path(), ".env.example", "STRIPE_KEY=sk_test_xxx\n");
        let hits = scan_repo(dir.path()).unwrap();
        let mut got = names(&hits);
        got.sort();
        assert_eq!(got, vec!["API_KEY", "DATABASE_URL", "STRIPE_KEY"]);
    }

    #[test]
    fn scans_package_json_var_expansions() {
        let dir = TempDir::new().unwrap();
        write(
            dir.path(),
            "package.json",
            r#"{
  "scripts": {
    "build": "echo ${BUILD_TOKEN}"
  }
}
"#,
        );
        let hits = scan_repo(dir.path()).unwrap();
        assert_eq!(names(&hits), vec!["BUILD_TOKEN"]);
    }

    #[test]
    fn skips_target_node_modules_and_hidden_dirs() {
        let dir = TempDir::new().unwrap();
        write(
            dir.path(),
            "target/build.rs",
            r#"std::env::var("SHOULD_BE_IGNORED");"#,
        );
        write(
            dir.path(),
            "node_modules/x/index.js",
            r#"process.env.SHOULD_ALSO_IGNORE"#,
        );
        write(dir.path(), ".cache/x.py", r#"os.getenv("HIDDEN_DIR_SKIP")"#);
        write(dir.path(), "src/main.rs", r#"std::env::var("KEEP");"#);
        let hits = scan_repo(dir.path()).unwrap();
        assert_eq!(names(&hits), vec!["KEEP"]);
    }

    #[test]
    fn ignores_lines_in_line_comments() {
        let dir = TempDir::new().unwrap();
        write(
            dir.path(),
            "src/main.rs",
            r#"
// std::env::var("COMMENTED_OUT")
fn main() { let _ = std::env::var("LIVE_VAR"); }
"#,
        );
        let hits = scan_repo(dir.path()).unwrap();
        assert_eq!(names(&hits), vec!["LIVE_VAR"]);
    }

    #[test]
    fn deduplicates_hits_per_unique_triple() {
        let dir = TempDir::new().unwrap();
        write(
            dir.path(),
            "src/x.ts",
            "const a = process.env.SAME;\nconst b = process.env.SAME;\n",
        );
        let hits = scan_repo(dir.path()).unwrap();
        // Two distinct lines, one var → two hits with different
        // line numbers.
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].line, 1);
        assert_eq!(hits[1].line, 2);
    }

    #[test]
    fn returns_sorted_output_by_var_then_file_then_line() {
        let dir = TempDir::new().unwrap();
        write(dir.path(), "a.py", r#"os.getenv("Z")"#);
        write(dir.path(), "b.py", r#"os.getenv("A")"#);
        let hits = scan_repo(dir.path()).unwrap();
        assert_eq!(names(&hits), vec!["A", "Z"]);
    }
}
