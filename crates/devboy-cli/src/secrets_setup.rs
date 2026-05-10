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
// Path proposer (P26.3)
// =============================================================================

/// One ADR-020 path proposal derived from an [`EnvVarHit`].
/// `Skip` means the env-var is not a credential (e.g.
/// `JIRA_USER`, `LANG`); `Path` carries the proposed path
/// plus the inferred provider identifier, which the wizard
/// uses to pick a [`devboy_token_catalog::ProviderCatalog`]
/// when the user opens the provision dialog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProposedPath {
    /// The wizard should add this path to the manifest.
    Path {
        /// Source observation that triggered the proposal.
        env_var: String,
        /// Suggested ADR-020 path (`<scope>/<provider>/<purpose>`).
        path: String,
        /// `true` when the provider segment matches a bundled
        /// catalog `provider_id` — the GUI can offer the
        /// matching variant directly.
        provider_known: bool,
    },
    /// Variable is not a credential — surface it to the user
    /// so they can confirm the skip but do not propose a
    /// path.
    Skip {
        env_var: String,
        /// One-line reason rendered in the wizard's review
        /// pane (e.g. `not a secret — usually a username`).
        reason: String,
    },
}

/// Reduce a slice of [`EnvVarHit`]s to one [`ProposedPath`]
/// per distinct `var_name`. The first occurrence wins for
/// `env_var` provenance — call sites typically already
/// dedupe per (file, line) via [`scan_repo`], so the input
/// length is small.
///
/// `known_providers` is the set of `provider_id` values from
/// the active token catalog — usually
/// `bundled_catalogs().iter().map(|c| c.provider_id.clone())`.
/// Empty list disables provider-detection (everything routes
/// through the `personal/` fallback).
pub fn propose_paths(hits: &[EnvVarHit], known_providers: &[String]) -> Vec<ProposedPath> {
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut out: Vec<ProposedPath> = Vec::new();
    for hit in hits {
        if !seen.insert(hit.var_name.clone()) {
            continue;
        }
        out.push(propose_one(&hit.var_name, known_providers));
    }
    out
}

/// Single-var proposer — exposed for tests and the wizard's
/// "propose for this manual entry" path.
pub fn propose_one(var: &str, known_providers: &[String]) -> ProposedPath {
    if let Some(reason) = non_secret_reason(var) {
        return ProposedPath::Skip {
            env_var: var.to_owned(),
            reason: reason.to_owned(),
        };
    }
    let var_lower = var.to_lowercase();
    let (provider, provider_known) = pick_provider(&var_lower, known_providers);
    let purpose = pick_purpose(&var_lower, &provider);
    let scope = if provider_known { "team" } else { "personal" };
    ProposedPath::Path {
        env_var: var.to_owned(),
        path: format!("{scope}/{provider}/{purpose}"),
        provider_known,
    }
}

/// Names that are clearly NOT secrets — usernames, emails,
/// hostnames, ports, locale toggles. Returning `Some(reason)`
/// makes the proposer emit a `Skip` proposal so the user can
/// see the rejected name and confirm.
fn non_secret_reason(var: &str) -> Option<&'static str> {
    let v = var.to_uppercase();
    // Suffix-based: e.g. `JIRA_USER`, `JIRA_USERNAME`.
    const NON_SECRET_SUFFIXES: &[&str] = &[
        "_USER",
        "_USERNAME",
        "_LOGIN",
        "_EMAIL",
        "_HOST",
        "_HOSTNAME",
        "_PORT",
        "_URL",
        "_REGION",
        "_PROJECT",
        "_BUCKET",
        "_NAMESPACE",
        "_ID",
    ];
    for suf in NON_SECRET_SUFFIXES {
        if v.ends_with(suf) {
            return Some("not a secret — usually a username / hostname / id");
        }
    }
    // Exact-match list for short standalone names.
    match v.as_str() {
        "USER" | "USERNAME" | "EMAIL" | "HOST" | "HOSTNAME" | "PORT" | "URL" | "LANG"
        | "LOCALE" | "TZ" | "HOME" | "PATH" | "PWD" | "SHELL" | "TERM" => {
            Some("not a secret — environment / machine variable")
        }
        _ => None,
    }
}

/// Pick `<provider>` for the path. Walks the env var's
/// underscore-separated parts, lower-cased, and returns the
/// first part that matches a known `provider_id` from the
/// catalog (case-insensitively). Falls back to the first
/// alphabetic part for the `personal/` scope.
fn pick_provider(var_lower: &str, known: &[String]) -> (String, bool) {
    let parts: Vec<&str> = var_lower.split('_').filter(|p| !p.is_empty()).collect();
    for p in &parts {
        for k in known {
            if k.eq_ignore_ascii_case(p) {
                return (k.clone(), true);
            }
        }
    }
    // Hardcoded provider hints for cases the bundled catalog
    // does not cover yet (the catalog is intentionally small
    // — issue #258 will keep it fresh). These aliases keep
    // the proposer useful before that lands.
    const HINTS: &[(&str, &str)] = &[
        ("jira", "jira"),
        ("gitlab", "gitlab"),
        ("github", "github"),
        ("slack", "slack"),
        ("openai", "openai"),
        ("anthropic", "anthropic"),
        ("kimi", "kimi"),
        ("clickup", "clickup"),
        ("confluence", "confluence"),
        ("stripe", "stripe"),
        ("aws", "aws"),
        ("gcp", "gcp"),
    ];
    for p in &parts {
        for (alias, canonical) in HINTS {
            if alias == p {
                // Match found in hints — `provider_known` is
                // false because the bundled catalog does not
                // know about it yet, but the path uses the
                // canonical name so a future catalog update
                // matches without rewriting the manifest.
                return ((*canonical).to_owned(), false);
            }
        }
    }
    // Last resort: first alphabetic-only part.
    let fallback = parts
        .iter()
        .find(|p| p.chars().all(|c| c.is_ascii_alphabetic()))
        .map(|s| (*s).to_owned())
        .unwrap_or_else(|| "unknown".to_owned());
    (fallback, false)
}

/// Pick `<purpose>` for the path. Strips the provider segment
/// and any trailing `_TOKEN` / `_KEY` / `_SECRET` noise; what
/// remains is the purpose. Defaults to `api-key` when the
/// remainder is empty, mirroring the bundled catalogs'
/// canonical name for "the only credential the provider has".
fn pick_purpose(var_lower: &str, provider: &str) -> String {
    let parts: Vec<&str> = var_lower
        .split('_')
        .filter(|p| !p.is_empty() && *p != provider)
        .collect();
    // Remove trailing noise words. `key` is intentionally
    // NOT in this list — `api-key` is the canonical purpose
    // for "the only credential a provider has", so an
    // `OPENAI_API_KEY` env-var should land as
    // `team/openai/api-key`, not `team/openai/api`.
    const TRAILING_NOISE: &[&str] = &["token", "secret", "credential", "credentials"];
    let mut cleaned: Vec<&str> = parts;
    while cleaned
        .last()
        .map(|p| TRAILING_NOISE.contains(p))
        .unwrap_or(false)
    {
        cleaned.pop();
    }
    if cleaned.is_empty() {
        return "api-key".to_owned();
    }
    cleaned.join("-")
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

    // ====== propose_paths fixtures (P26.3) ======================

    fn known() -> Vec<String> {
        vec!["openai".into(), "github".into(), "kimi".into()]
    }

    fn assert_path(p: &ProposedPath, want_path: &str, want_known: bool) {
        match p {
            ProposedPath::Path {
                path,
                provider_known,
                ..
            } => {
                assert_eq!(path, want_path, "{p:?}");
                assert_eq!(*provider_known, want_known, "{p:?}");
            }
            ProposedPath::Skip { .. } => panic!("expected Path, got Skip: {p:?}"),
        }
    }

    fn assert_skip(p: &ProposedPath) {
        assert!(matches!(p, ProposedPath::Skip { .. }), "{p:?}");
    }

    #[test]
    fn proposer_jira_token_to_team_jira_api_key() {
        // No `jira` in bundled — provider_known=false, but
        // the canonical hint produces the right path.
        let p = propose_one("JIRA_TOKEN", &known());
        assert_path(&p, "personal/jira/api-key", false);
    }

    #[test]
    fn proposer_skips_jira_user() {
        assert_skip(&propose_one("JIRA_USER", &known()));
        assert_skip(&propose_one("JIRA_EMAIL", &known()));
        assert_skip(&propose_one("JIRA_USERNAME", &known()));
    }

    #[test]
    fn proposer_openai_api_key_known_provider() {
        // `openai` is in `known` → provider_known=true,
        // scope=team.
        let p = propose_one("OPENAI_API_KEY", &known());
        assert_path(&p, "team/openai/api-key", true);
    }

    #[test]
    fn proposer_gitlab_token_routes_to_gitlab_via_hints() {
        // `gitlab` is not in our `known` slice, so it routes
        // through the hardcoded hints — provider_known=false,
        // scope=personal, purpose collapses to `api-key`
        // because GITLAB_TOKEN has no extra purpose words.
        let p = propose_one("GITLAB_TOKEN", &known());
        assert_path(&p, "personal/gitlab/api-key", false);
    }

    #[test]
    fn proposer_strips_trailing_token_key_secret_words() {
        // `OPENAI_API_KEY` keeps `api`; `_KEY` drops; the
        // result is `api-key` because we collapse trailing
        // noise iteratively. Symmetric for `_SECRET`.
        let p = propose_one("OPENAI_SECRET", &known());
        assert_path(&p, "team/openai/api-key", true);
    }

    #[test]
    fn proposer_unknown_provider_falls_back_to_personal() {
        // `WEIRD_TOKEN` — first part `weird` matches no
        // catalog and no hint. Fallback `personal/weird/...`,
        // purpose collapses to `api-key`.
        let p = propose_one("WEIRD_TOKEN", &known());
        assert_path(&p, "personal/weird/api-key", false);
    }

    #[test]
    fn proposer_keeps_purpose_when_distinct_from_provider() {
        // `GITHUB_DEPLOY_TOKEN` — provider=github (known),
        // purpose=deploy (token stripped as noise).
        let p = propose_one("GITHUB_DEPLOY_TOKEN", &known());
        assert_path(&p, "team/github/deploy", true);
    }

    #[test]
    fn proposer_skips_machine_environment_vars() {
        // Standalone names like `HOME` / `PATH` / `PORT` are
        // never secrets.
        assert_skip(&propose_one("HOME", &known()));
        assert_skip(&propose_one("PATH", &known()));
        assert_skip(&propose_one("PORT", &known()));
    }

    #[test]
    fn proposer_dedupes_repeated_var_across_hits() {
        // Same env-var, two distinct files / lines — one
        // proposal in the result.
        let hits = vec![
            EnvVarHit {
                var_name: "OPENAI_API_KEY".into(),
                file: PathBuf::from("a.py"),
                line: 1,
            },
            EnvVarHit {
                var_name: "OPENAI_API_KEY".into(),
                file: PathBuf::from("b.py"),
                line: 12,
            },
        ];
        let proposals = propose_paths(&hits, &known());
        assert_eq!(proposals.len(), 1);
        assert_path(&proposals[0], "team/openai/api-key", true);
    }

    #[test]
    fn proposer_handles_multi_segment_purpose() {
        // `STRIPE_WEBHOOK_SIGNING_SECRET` — provider=stripe
        // (hint), purpose=webhook-signing (after stripping
        // trailing `_SECRET`).
        let p = propose_one("STRIPE_WEBHOOK_SIGNING_SECRET", &known());
        assert_path(&p, "personal/stripe/webhook-signing", false);
    }
}
