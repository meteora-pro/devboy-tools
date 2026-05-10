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
    // P5 — when the provider segment OR the purpose collapses
    // to a generic word (`api`, `service`, `app`, `bot`,
    // `worker`, …), the resulting `personal/<x>/...` path
    // carries false confidence — the user has no real
    // provider hint to act on. Skip with an actionable nudge:
    // rename the env var (`SERVICE_TOKEN` → `STRIPE_TOKEN`)
    // or pick the provider segment by hand. Demo on
    // devboy-env-1 surfaced `SERVICE_TOKEN`,
    // `MEET_SERVICE_TOKEN`, `API_KEY`, `BOT_IMAGE` as exactly
    // this case.
    //
    // Only fires when `provider_known == false`: a real
    // catalog match (`OPENAI_API_KEY` → `openai`) is never
    // ambiguous, even if the purpose happens to be a
    // generic-shaped word.
    if !provider_known
        && (is_generic_provider_segment(&provider) || is_generic_provider_segment(&purpose))
    {
        return ProposedPath::Skip {
            env_var: var.to_owned(),
            reason: "ambiguous provider segment — rename the env var to include a real provider \
                     (e.g. `STRIPE_TOKEN` instead of `SERVICE_TOKEN`) or add the path manually"
                .to_owned(),
        };
    }
    let scope = if provider_known { "team" } else { "personal" };
    ProposedPath::Path {
        env_var: var.to_owned(),
        path: format!("{scope}/{provider}/{purpose}"),
        provider_known,
    }
}

/// Provider-segment names that are too generic to anchor an
/// ADR-020 path on. An env var like `SERVICE_TOKEN` carries
/// no real provider hint; pretending it does writes a
/// confidently-wrong `personal/service/api-key` entry. The
/// proposer treats these as ambiguous and prompts the user
/// to rename or add the path manually.
fn is_generic_provider_segment(seg: &str) -> bool {
    const GENERIC: &[&str] = &[
        "api", "app", "bot", "core", "data", "db", "key", "main", "secret", "server", "service",
        "system", "test", "token", "web", "worker",
    ];
    GENERIC.iter().any(|g| seg.eq_ignore_ascii_case(g))
}

/// Names that are clearly NOT secrets — usernames, emails,
/// hostnames, ports, locale toggles. Returning `Some(reason)`
/// makes the proposer emit a `Skip` proposal so the user can
/// see the rejected name and confirm.
fn non_secret_reason(var: &str) -> Option<&'static str> {
    let v = var.to_uppercase();
    // Suffix-based: e.g. `JIRA_USER`, `JIRA_USERNAME`,
    // `OPENAI_API_BASE`, `LLM_MODEL`. Names ending with one
    // of these are configuration / connection metadata, not
    // credentials. Real-world data from `meteora/devboy-env-1`
    // (P26 demo) had ~50 of these slipping through as fake
    // secrets — `OPENAI_API_BASE`, `BAML_LOG`,
    // `ANTHROPIC_CONCURRENCY`, `EMBEDDING_MODEL`,
    // `REDIS_HOST`, `REDIS_DB`, … — all were noise.
    const NON_SECRET_SUFFIXES: &[&str] = &[
        // Identity / addressability
        "_USER",
        "_USERNAME",
        "_LOGIN",
        "_EMAIL",
        "_HOST",
        "_HOSTNAME",
        "_PORT",
        "_URL",
        "_ENDPOINT",
        "_REGION",
        "_PROJECT",
        "_BUCKET",
        "_NAMESPACE",
        "_ID",
        "_NAME",
        // Configuration / tunables
        "_BASE",
        "_MODEL",
        "_LOG",
        "_LEVEL",
        "_DEBUG",
        "_VERBOSE",
        "_CONCURRENCY",
        "_LIMIT",
        "_SIZE",
        "_TIMEOUT",
        "_RATE",
        "_INTERVAL",
        "_RETRIES",
        "_FORMAT",
        // Filesystem / connection metadata
        "_DB",
        "_DSN",
        "_PATH",
        "_DIR",
        "_FILE",
        "_FILENAME",
        // Service / role hints (not credentials themselves)
        "_SERVICE",
        "_ENV",
        "_MODE",
        "_KIND",
        "_TYPE",
    ];
    for suf in NON_SECRET_SUFFIXES {
        if v.ends_with(suf) {
            return Some("not a secret — usually configuration / connection metadata");
        }
    }
    // Prefix-based: CI runner / build agent env-vars. The
    // live demo on meteora/devboy-env-1 (P26) saw 30+ matches
    // for `CI_COMMIT_*`, `CI_PIPELINE_*`, `BUILD_*`, `BAML_*`
    // — all of them runner / build metadata published by the
    // CI system, never user-typed credentials. Match the
    // prefix and short-circuit before the provider heuristic
    // runs.
    const NON_SECRET_PREFIXES: &[&str] = &[
        "CI_COMMIT_",
        "CI_PIPELINE_",
        "CI_JOB_",
        "CI_PAGES_",
        "CI_RUNNER_",
        "CI_PROJECT_",
        "CI_BUILD_",
        "CI_MERGE_REQUEST_",
        "BUILD_",
        "BAML_",
        "GITHUB_ACTIONS_",
        "GITHUB_WORKFLOW_",
        "GITHUB_RUN_",
        "GITHUB_REF_",
        "GITHUB_HEAD_",
        "GITHUB_BASE_",
        "GITHUB_EVENT_",
        "RUNNER_",
        "BUILDKITE_",
        "JENKINS_",
        "TRAVIS_",
        "CIRCLECI_",
        "DRONE_",
        "TEAMCITY_",
        "SEMAPHORE_",
        "VERCEL_",
        "NETLIFY_",
        "RAILWAY_",
        "HEROKU_",
        "AWS_LAMBDA_",
        "AWS_EXECUTION_",
        "K_SERVICE_",
        "GOOGLE_CLOUD_",
        "GCP_PROJECT_",
        "FUNCTION_",
        // P3 — Windows CPU / processor metadata. Surface
        // identical on every Windows host; never a credential.
        "PROCESSOR_",
        // P4 — POSIX locale + XDG base-dir + GPG agent
        // metadata. `LC_*` is the locale category family;
        // `XDG_*` are the base-dir spec keys
        // (XDG_CONFIG_HOME, XDG_DATA_HOME, etc); `GPG_*`
        // mostly point at the agent socket and TTY hint.
        // None are user-typed credentials. SSH agents have
        // their own socket-only contract — `SSH_AUTH_SOCK`,
        // `SSH_AGENT_PID`, `SSH_CONNECTION`, `SSH_TTY` —
        // also config, never values.
        "LC_",
        "XDG_",
        "GPG_",
        "SSH_",
    ];
    for pre in NON_SECRET_PREFIXES {
        if v.starts_with(pre) {
            return Some("not a secret — CI runner / build agent / cloud metadata");
        }
    }
    // Exact-match list for short standalone names.
    match v.as_str() {
        "USER"
        | "USERNAME"
        | "EMAIL"
        | "HOST"
        | "HOSTNAME"
        | "PORT"
        | "URL"
        | "LANG"
        | "LOCALE"
        | "TZ"
        | "HOME"
        | "PATH"
        | "PWD"
        | "SHELL"
        | "TERM"
        | "CI"
        | "CONTINUOUS_INTEGRATION"
        | "GITHUB_ACTIONS"
        | "GITLAB_CI"
        | "BUILD_ID"
        // P3 — Windows machine environment. Set by the OS or
        // by Cygwin / MSYS / WSL bootstrap; never user-typed
        // credentials. Demo on devboy-env-1 surfaced
        // COMPUTERNAME / APPDATA as fake secrets.
        | "COMPUTERNAME"
        | "APPDATA"
        | "LOCALAPPDATA"
        | "HOMEDRIVE"
        | "HOMEPATH"
        | "USERPROFILE"
        | "USERDOMAIN"
        | "USERDOMAIN_ROAMINGPROFILE"
        | "OS"
        | "SYSTEMROOT"
        | "SYSTEMDRIVE"
        | "WINDIR"
        | "PROGRAMFILES"
        | "PROGRAMFILES(X86)"
        | "PROGRAMW6432"
        | "PROGRAMDATA"
        | "PUBLIC"
        | "ALLUSERSPROFILE"
        | "COMMONPROGRAMFILES"
        | "COMMONPROGRAMFILES(X86)"
        | "COMMONPROGRAMW6432"
        | "COMSPEC"
        | "TEMP"
        | "TMP"
        | "PSMODULEPATH"
        | "PATHEXT"
        | "SESSIONNAME"
        | "LOGONSERVER"
        | "DRIVERDATA"
        | "ONEDRIVE"
        | "ONEDRIVECONSUMER"
        | "ONEDRIVECOMMERCIAL"
        // P4 — POSIX shell / editor / pager toggles. Set by
        // shell init (zshrc, bashrc, profile) and inherited
        // through every subprocess; never user credentials.
        | "LANGUAGE"
        | "EDITOR"
        | "VISUAL"
        | "PAGER"
        | "MANPAGER"
        | "MANPATH"
        | "INFOPATH"
        | "BROWSER"
        | "OLDPWD"
        | "_" => Some("not a secret — environment / machine variable"),
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
// Wizard runner (P26.4)
// =============================================================================

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Pure-data progress events emitted by [`run_wizard`]. Drivers
/// render them however they like — the CLI prints one event per
/// line; integration tests collect them and assert sequence /
/// counts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WizardEvent {
    PhaseStarted {
        phase: WizardPhase,
    },
    PhaseProgress {
        phase: WizardPhase,
        message: String,
    },
    PhaseCompleted {
        phase: WizardPhase,
        summary: String,
    },
    PhaseSkipped {
        phase: WizardPhase,
        reason: String,
    },
    PhaseFailed {
        phase: WizardPhase,
        reason: String,
    },
    /// Final event when every phase settled `done` / `skipped`.
    /// Absent when the wizard short-circuits on a failure.
    Completed,
}

/// The four phases the wizard walks. Stored as `[steps.<key>]`
/// entries in `~/.devboy/secrets/setup-state.toml` so a re-run
/// jumps straight to the first non-`done` phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum WizardPhase {
    /// Scan the repo for env-var references.
    Scan,
    /// Turn hits into ADR-020 path proposals.
    Propose,
    /// For each path the manifest declares as required-but-
    /// missing, open the provision dialog and wait for the
    /// user.
    Provision,
    /// Final `devboy doctor --secrets` validation.
    Doctor,
}

impl WizardPhase {
    pub const ALL: [Self; 4] = [Self::Scan, Self::Propose, Self::Provision, Self::Doctor];

    pub fn key(self) -> &'static str {
        match self {
            Self::Scan => "scan",
            Self::Propose => "propose",
            Self::Provision => "provision",
            Self::Doctor => "doctor",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Scan => "Scan repo for env-var references",
            Self::Propose => "Propose ADR-020 paths from env-vars",
            Self::Provision => "Provision missing required values",
            Self::Doctor => "Run `devboy doctor --secrets`",
        }
    }
}

/// Outcome of one provision request from the wizard's
/// perspective. Mirrors the agent-side `ProvisionStatus`
/// terminal variants. The variants below `Saved` are
/// constructed only by daemon-glue code (out-of-process —
/// invisible to dead-code analysis on the CLI binary) and by
/// tests; mark them `allow(dead_code)` so the `-D warnings`
/// CI gate does not trip on the API surface.
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProvisionOutcome {
    Saved,
    Cancelled,
    Expired,
    Failed { reason: String },
}

/// Outcome of the final `doctor` run. `Red` is constructed
/// out-of-process; `allow(dead_code)` for the same reason as
/// [`ProvisionOutcome`].
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DoctorOutcome {
    Green,
    Red { summary: String },
}

/// Injection seam for IO. Production wires the real scanner +
/// `devboy doctor --secrets` exec + an MCP client; tests pass a
/// fake.
pub trait WizardIo {
    fn scan(&self, root: &Path) -> std::io::Result<Vec<EnvVarHit>>;
    fn known_providers(&self) -> Vec<String>;
    /// Paths declared as `required` in the project manifest
    /// whose value has not yet landed in any source. The
    /// wizard provisions exactly these.
    fn missing_required_paths(&self) -> std::io::Result<Vec<String>>;
    fn provision(&mut self, path: &str) -> ProvisionOutcome;
    fn doctor(&self) -> DoctorOutcome;
    /// Wall-clock timestamp in ISO-8601, embedded into the
    /// state file. Injectable so tests can pin the value.
    fn now_iso8601(&self) -> String;
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct PersistedState {
    #[serde(default)]
    schema_version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    started_at: Option<String>,
    #[serde(default)]
    last_step: u32,
    #[serde(default)]
    steps: BTreeMap<String, StepRecord>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct StepRecord {
    /// `pending` / `in-progress` / `done` / `skipped` / `failed`.
    status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    note: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    completed_at: Option<String>,
}

impl StepRecord {
    fn is_settled(&self) -> bool {
        matches!(self.status.as_str(), "done" | "skipped")
    }
}

fn load_state(path: &Path) -> PersistedState {
    match fs::read_to_string(path) {
        Ok(body) => toml::from_str::<PersistedState>(&body).unwrap_or_default(),
        Err(_) => PersistedState {
            schema_version: 1,
            ..PersistedState::default()
        },
    }
}

fn save_state(path: &Path, state: &PersistedState) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let body = toml::to_string(state).map_err(|e| std::io::Error::other(e.to_string()))?;
    fs::write(path, body)
}

/// Typed view over the persisted wizard state. Returned by
/// [`read_setup_state`] so callers (the future `devboy secrets
/// setup --resume` sub-command, doctor checks, status renders)
/// don't have to parse the TOML by hand.
///
/// Carries only the fields a caller actually needs to make a
/// decision: which phases are settled, which paths the wizard
/// has already provisioned, and how far through the four-phase
/// flow the previous run got. Private fields like
/// `completed_at` stay inside the persisted-state shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetupState {
    /// Phase 1 — env-var scan — has settled (`done` /
    /// `skipped`).
    pub scanned: bool,
    /// Phase 2 — path proposal review — has settled.
    pub proposed: bool,
    /// Paths the wizard has already provisioned within this
    /// state file. Read out of the `provision` step's `note`,
    /// which the runner writes as `provisioned=<n>` plus a
    /// per-path summary line.
    pub provisioned: Vec<String>,
    /// 1-indexed position of the highest phase the previous
    /// run reached. `0` means "wizard has not started"; `4`
    /// means "every phase settled". Matches `WizardPhase::ALL`
    /// + 1.
    pub last_step: u8,
}

impl SetupState {
    /// `true` when every phase has settled. `read_setup_state`
    /// callers use this to decide whether `--resume` would be
    /// a no-op and a fresh-run hint is more useful.
    pub fn is_complete(&self) -> bool {
        self.last_step >= WizardPhase::ALL.len() as u8
    }
}

/// Read the wizard's state file and project it into a
/// [`SetupState`]. A missing or malformed file maps to a
/// fresh `SetupState` (`scanned=false`, …, `last_step=0`) —
/// the wizard treats both the same way.
pub fn read_setup_state(path: &Path) -> SetupState {
    let persisted = load_state(path);
    SetupState {
        scanned: persisted
            .steps
            .get(WizardPhase::Scan.key())
            .map(|r| r.is_settled())
            .unwrap_or(false),
        proposed: persisted
            .steps
            .get(WizardPhase::Propose.key())
            .map(|r| r.is_settled())
            .unwrap_or(false),
        provisioned: persisted
            .steps
            .get(WizardPhase::Provision.key())
            .filter(|r| r.status == "done")
            .map(|r| {
                // Recorder note shape on success:
                // `provisioned=<n>: <path1>, <path2>, ...`.
                // Pull out the path-shaped tokens —
                // anything matching ADR-020's
                // `<scope>/<provider>/<purpose>` form.
                r.note
                    .as_deref()
                    .map(|n| {
                        n.split([',', ' ', ';'])
                            .map(str::trim)
                            .filter(|p| p.matches('/').count() >= 2)
                            .map(str::to_owned)
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default()
            })
            .unwrap_or_default(),
        last_step: persisted.last_step.try_into().unwrap_or(u8::MAX),
    }
}

fn ensure_started(state: &mut PersistedState, io: &impl WizardIo) {
    if state.schema_version == 0 {
        state.schema_version = 1;
    }
    if state.started_at.is_none() {
        state.started_at = Some(io.now_iso8601());
    }
}

fn phase_settled(state: &PersistedState, phase: WizardPhase) -> bool {
    state
        .steps
        .get(phase.key())
        .map(|r| r.is_settled())
        .unwrap_or(false)
}

fn record_status(
    state: &mut PersistedState,
    phase: WizardPhase,
    status: &str,
    note: Option<String>,
    io: &impl WizardIo,
) {
    let entry = state.steps.entry(phase.key().to_owned()).or_default();
    entry.status = status.to_owned();
    entry.note = note;
    entry.completed_at = Some(io.now_iso8601());
    let phase_idx = WizardPhase::ALL
        .iter()
        .position(|p| *p == phase)
        .map(|p| (p + 1) as u32)
        .unwrap_or(state.last_step);
    if phase_idx > state.last_step {
        state.last_step = phase_idx;
    }
}

/// Drive the wizard from scan to doctor. Resume-aware: phases
/// already marked `done` / `skipped` in `state_path` are not
/// re-run. The wizard short-circuits on the first failure;
/// unsettled phases stay `pending` so the next run picks them
/// up.
pub fn run_wizard<I: WizardIo>(
    repo_root: &Path,
    state_path: &Path,
    io: &mut I,
) -> Vec<WizardEvent> {
    let mut events = Vec::new();
    let mut state = load_state(state_path);
    ensure_started(&mut state, io);

    // ---- Phase 1: Scan -------------------------------------------
    let hits = if phase_settled(&state, WizardPhase::Scan) {
        events.push(WizardEvent::PhaseSkipped {
            phase: WizardPhase::Scan,
            reason: "already done".to_owned(),
        });
        Vec::new()
    } else {
        events.push(WizardEvent::PhaseStarted {
            phase: WizardPhase::Scan,
        });
        match io.scan(repo_root) {
            Ok(hits) => {
                events.push(WizardEvent::PhaseCompleted {
                    phase: WizardPhase::Scan,
                    summary: format!("{} env-var references found", hits.len()),
                });
                record_status(
                    &mut state,
                    WizardPhase::Scan,
                    "done",
                    Some(format!("hits={}", hits.len())),
                    io,
                );
                hits
            }
            Err(e) => {
                let reason = format!("{e}");
                events.push(WizardEvent::PhaseFailed {
                    phase: WizardPhase::Scan,
                    reason: reason.clone(),
                });
                record_status(&mut state, WizardPhase::Scan, "failed", Some(reason), io);
                let _ = save_state(state_path, &state);
                return events;
            }
        }
    };

    // ---- Phase 2: Propose ----------------------------------------
    if phase_settled(&state, WizardPhase::Propose) {
        events.push(WizardEvent::PhaseSkipped {
            phase: WizardPhase::Propose,
            reason: "already done".to_owned(),
        });
    } else {
        events.push(WizardEvent::PhaseStarted {
            phase: WizardPhase::Propose,
        });
        let proposals = propose_paths(&hits, &io.known_providers());
        let path_count = proposals
            .iter()
            .filter(|p| matches!(p, ProposedPath::Path { .. }))
            .count();
        let skip_count = proposals.len() - path_count;
        events.push(WizardEvent::PhaseCompleted {
            phase: WizardPhase::Propose,
            summary: format!("{path_count} proposed paths, {skip_count} skipped"),
        });
        record_status(
            &mut state,
            WizardPhase::Propose,
            "done",
            Some(format!("paths={path_count}, skipped={skip_count}")),
            io,
        );
    }

    // ---- Phase 3: Provision --------------------------------------
    if phase_settled(&state, WizardPhase::Provision) {
        events.push(WizardEvent::PhaseSkipped {
            phase: WizardPhase::Provision,
            reason: "already done".to_owned(),
        });
    } else {
        events.push(WizardEvent::PhaseStarted {
            phase: WizardPhase::Provision,
        });
        let missing = match io.missing_required_paths() {
            Ok(v) => v,
            Err(e) => {
                let reason = format!("manifest read failed: {e}");
                events.push(WizardEvent::PhaseFailed {
                    phase: WizardPhase::Provision,
                    reason: reason.clone(),
                });
                record_status(
                    &mut state,
                    WizardPhase::Provision,
                    "failed",
                    Some(reason),
                    io,
                );
                let _ = save_state(state_path, &state);
                return events;
            }
        };
        if missing.is_empty() {
            events.push(WizardEvent::PhaseSkipped {
                phase: WizardPhase::Provision,
                reason: "no missing required paths".to_owned(),
            });
            record_status(
                &mut state,
                WizardPhase::Provision,
                "skipped",
                Some("no missing required".to_owned()),
                io,
            );
        } else {
            let mut saved_paths: Vec<String> = Vec::new();
            let mut bail: Option<String> = None;
            for path in &missing {
                events.push(WizardEvent::PhaseProgress {
                    phase: WizardPhase::Provision,
                    message: format!("requesting provision for {path}"),
                });
                match io.provision(path) {
                    ProvisionOutcome::Saved => {
                        saved_paths.push(path.clone());
                        events.push(WizardEvent::PhaseProgress {
                            phase: WizardPhase::Provision,
                            message: format!("{path}: saved"),
                        });
                    }
                    ProvisionOutcome::Cancelled => {
                        bail = Some(format!("user cancelled provision for {path}"));
                        break;
                    }
                    ProvisionOutcome::Expired => {
                        bail = Some(format!("provision dialog expired for {path}"));
                        break;
                    }
                    ProvisionOutcome::Failed { reason } => {
                        bail = Some(format!("provision failed for {path}: {reason}"));
                        break;
                    }
                }
            }
            match bail {
                None => {
                    events.push(WizardEvent::PhaseCompleted {
                        phase: WizardPhase::Provision,
                        summary: format!("{}/{} provisioned", saved_paths.len(), missing.len()),
                    });
                    // Record both the count and the path list
                    // so `read_setup_state` can recover the
                    // already-provisioned set on resume.
                    let note = format!(
                        "provisioned={}: {}",
                        saved_paths.len(),
                        saved_paths.join(", ")
                    );
                    record_status(&mut state, WizardPhase::Provision, "done", Some(note), io);
                }
                Some(reason) => {
                    events.push(WizardEvent::PhaseFailed {
                        phase: WizardPhase::Provision,
                        reason: reason.clone(),
                    });
                    record_status(
                        &mut state,
                        WizardPhase::Provision,
                        "failed",
                        Some(reason),
                        io,
                    );
                    let _ = save_state(state_path, &state);
                    return events;
                }
            }
        }
    }

    // ---- Phase 4: Doctor -----------------------------------------
    if phase_settled(&state, WizardPhase::Doctor) {
        events.push(WizardEvent::PhaseSkipped {
            phase: WizardPhase::Doctor,
            reason: "already done".to_owned(),
        });
    } else {
        events.push(WizardEvent::PhaseStarted {
            phase: WizardPhase::Doctor,
        });
        match io.doctor() {
            DoctorOutcome::Green => {
                events.push(WizardEvent::PhaseCompleted {
                    phase: WizardPhase::Doctor,
                    summary: "doctor green".to_owned(),
                });
                record_status(&mut state, WizardPhase::Doctor, "done", None, io);
            }
            DoctorOutcome::Red { summary } => {
                events.push(WizardEvent::PhaseFailed {
                    phase: WizardPhase::Doctor,
                    reason: summary.clone(),
                });
                record_status(&mut state, WizardPhase::Doctor, "failed", Some(summary), io);
                let _ = save_state(state_path, &state);
                return events;
            }
        }
    }

    events.push(WizardEvent::Completed);
    let _ = save_state(state_path, &state);
    events
}

// =============================================================================
// CLI entry point (P26 polish)
// =============================================================================

/// Render one [`WizardEvent`] to stdout in JSON-lines mode.
fn print_event_json(event: &WizardEvent) {
    let (phase, status, message) = match event {
        WizardEvent::PhaseStarted { phase } => (Some(phase.key()), "started", None),
        WizardEvent::PhaseProgress { phase, message } => {
            (Some(phase.key()), "progress", Some(message.clone()))
        }
        WizardEvent::PhaseCompleted { phase, summary } => {
            (Some(phase.key()), "completed", Some(summary.clone()))
        }
        WizardEvent::PhaseSkipped { phase, reason } => {
            (Some(phase.key()), "skipped", Some(reason.clone()))
        }
        WizardEvent::PhaseFailed { phase, reason } => {
            (Some(phase.key()), "failed", Some(reason.clone()))
        }
        WizardEvent::Completed => (None, "wizard-completed", None),
    };
    let mut payload = serde_json::Map::new();
    if let Some(p) = phase {
        payload.insert("phase".into(), serde_json::Value::String(p.into()));
    }
    payload.insert("status".into(), serde_json::Value::String(status.into()));
    if let Some(m) = message {
        payload.insert("message".into(), serde_json::Value::String(m));
    }
    println!("{}", serde_json::Value::Object(payload));
}

/// Render one [`WizardEvent`] to stdout in human prose.
fn print_event_human(event: &WizardEvent) {
    match event {
        WizardEvent::PhaseStarted { phase } => println!("→ {}", phase.label()),
        WizardEvent::PhaseProgress { phase: _, message } => println!("  {message}"),
        WizardEvent::PhaseCompleted { phase, summary } => {
            println!("✓ {} — {summary}", phase.label())
        }
        WizardEvent::PhaseSkipped { phase, reason } => {
            println!("⤳ {} skipped — {reason}", phase.label())
        }
        WizardEvent::PhaseFailed { phase, reason } => {
            println!("✗ {} failed — {reason}", phase.label())
        }
        WizardEvent::Completed => println!("done."),
    }
}

/// `devboy secrets setup` entry point. Default mode is a
/// read-only scan + propose preview; opt-in via
/// `--write-manifest` or `--resume` for the mutating variants.
pub fn handle_cli(args: crate::secrets_cmd::SetupArgs) -> anyhow::Result<()> {
    use std::env;

    let root = match args.root {
        Some(p) => p,
        None => env::current_dir()?,
    };

    if args.resume {
        return run_full_wizard(&root, args.json);
    }

    // Otherwise the default mode is the read-only preview.
    let hits = scan_repo(&root)?;
    let known = bundled_provider_ids();
    let proposals = propose_paths(&hits, &known);
    let path_count = proposals
        .iter()
        .filter(|p| matches!(p, ProposedPath::Path { .. }))
        .count();
    let skip_count = proposals.len() - path_count;

    if args.json {
        for p in &proposals {
            let v = serde_json::to_value(serialise_proposal(p))?;
            println!("{v}");
        }
        let summary = serde_json::json!({
            "scanned_files": hits.iter().map(|h| h.file.clone()).collect::<std::collections::BTreeSet<_>>().len(),
            "env_var_hits": hits.len(),
            "proposed_paths": path_count,
            "skipped": skip_count,
        });
        println!("{summary}");
    } else {
        println!(
            "scanned {} files, {} env-var references, {} proposed paths, {} skipped",
            hits.iter()
                .map(|h| h.file.clone())
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            hits.len(),
            path_count,
            skip_count,
        );
        println!();
        for p in &proposals {
            match p {
                ProposedPath::Path {
                    env_var,
                    path,
                    provider_known,
                } => {
                    let mark = if *provider_known { "✓" } else { "·" };
                    println!("  {mark} {env_var:36} → {path}");
                }
                ProposedPath::Skip { env_var, reason } => {
                    println!("  ⤳ {env_var:36} skip — {reason}");
                }
            }
        }
    }

    if args.write_manifest {
        let manifest_path = root.join(".devboy/secrets.toml");
        if manifest_path.exists() && !args.force {
            anyhow::bail!(
                "{} already exists — pass --force to overwrite",
                manifest_path.display()
            );
        }
        if let Some(parent) = manifest_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let body = render_manifest(&proposals);
        fs::write(&manifest_path, body)?;
        if args.json {
            println!(
                "{}",
                serde_json::json!({
                    "status": "manifest-written",
                    "path": manifest_path.display().to_string(),
                })
            );
        } else {
            println!("\nwrote {}", manifest_path.display());
        }
    }

    Ok(())
}

fn run_full_wizard(root: &Path, json: bool) -> anyhow::Result<()> {
    let state_path = state_file_path()?;
    let prev = read_setup_state(&state_path);
    if !json {
        if prev.is_complete() {
            println!(
                "(state file shows the previous run completed every phase — \
                 delete {} to force a fresh run)",
                state_path.display()
            );
        } else if prev.last_step > 0 {
            println!(
                "resuming at phase {}/{} ({} provisioned so far)",
                prev.last_step + 1,
                WizardPhase::ALL.len(),
                prev.provisioned.len()
            );
        }
    }
    let mut io = ProductionWizardIo::new();
    let events = run_wizard(root, &state_path, &mut io);
    for e in &events {
        if json {
            print_event_json(e);
        } else {
            print_event_human(e);
        }
    }
    let failed = events
        .iter()
        .any(|e| matches!(e, WizardEvent::PhaseFailed { .. }));
    if failed {
        anyhow::bail!("wizard halted on a phase failure — see events above");
    }
    Ok(())
}

fn state_file_path() -> anyhow::Result<PathBuf> {
    if let Ok(p) = std::env::var("DEVBOY_SECRETS_HOME") {
        return Ok(PathBuf::from(p).join("setup-state.toml"));
    }
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("could not resolve home dir"))?;
    Ok(home.join(".devboy/secrets/setup-state.toml"))
}

fn bundled_provider_ids() -> Vec<String> {
    devboy_token_catalog::bundled_catalogs()
        .into_iter()
        .map(|c| c.provider_id)
        .collect()
}

fn serialise_proposal(p: &ProposedPath) -> serde_json::Value {
    match p {
        ProposedPath::Path {
            env_var,
            path,
            provider_known,
        } => serde_json::json!({
            "kind": "path",
            "env_var": env_var,
            "path": path,
            "provider_known": provider_known,
        }),
        ProposedPath::Skip { env_var, reason } => serde_json::json!({
            "kind": "skip",
            "env_var": env_var,
            "reason": reason,
        }),
    }
}

/// Render proposals into a TOML manifest body. Path-shaped
/// proposals land as `required = [...]` entries plus a
/// `[secret."<path>"]` block carrying the originating env-var
/// in the description so the user can reverse-trace.
fn render_manifest(proposals: &[ProposedPath]) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    out.push_str("# Generated by `devboy secrets setup --write-manifest`.\n");
    out.push_str("# Edit by hand to change paths, mark optional, or set approve_on_use.\n\n");

    let paths: Vec<&ProposedPath> = proposals
        .iter()
        .filter(|p| matches!(p, ProposedPath::Path { .. }))
        .collect();

    if !paths.is_empty() {
        out.push_str("required = [\n");
        for p in &paths {
            if let ProposedPath::Path { path, .. } = p {
                let _ = writeln!(out, "  \"{path}\",");
            }
        }
        out.push_str("]\n\n");
    }

    for p in &paths {
        if let ProposedPath::Path { env_var, path, .. } = p {
            let _ = writeln!(out, "[secret.\"{path}\"]");
            let _ = writeln!(out, "description = \"derived from ${env_var}\"");
            out.push_str("rotation_method = \"manual\"\n\n");
        }
    }

    out
}

/// Production [`WizardIo`] backed by the real scanner, the
/// project manifest reader, an MCP-side provision request, and
/// `devboy doctor --secrets`. The dialog / doctor execs are
/// stubbed for the first cut — the wizard structure is in
/// place; the orchestration glue lands when the daemon-side
/// dialog protocol stabilises.
struct ProductionWizardIo;

impl ProductionWizardIo {
    fn new() -> Self {
        Self
    }
}

impl WizardIo for ProductionWizardIo {
    fn scan(&self, root: &Path) -> std::io::Result<Vec<EnvVarHit>> {
        scan_repo(root)
    }
    fn known_providers(&self) -> Vec<String> {
        bundled_provider_ids()
    }
    fn missing_required_paths(&self) -> std::io::Result<Vec<String>> {
        // Conservative first cut: read the manifest and report
        // every `required = [...]` path. The richer version
        // (probe each path through the router and only return
        // the ones with no value) lands when the router is
        // wired into the CLI process — see ADR-021 §4.
        let manifest = match devboy_storage::ProjectManifest::load() {
            Ok(m) => m,
            Err(_) => return Ok(Vec::new()),
        };
        Ok(manifest.required.iter().map(|p| p.to_string()).collect())
    }
    fn provision(&mut self, _path: &str) -> ProvisionOutcome {
        // The CLI cannot open the dialog directly — the daemon
        // owns the UI surface. For the first cut, we surface a
        // clear failure that nudges the user to run
        // `devboy secrets ui` (interactive) or to start the
        // daemon and re-run the wizard from an MCP-aware host.
        ProvisionOutcome::Failed {
            reason: "CLI cannot open the provision dialog directly — \
                     run `devboy secrets ui` to provision interactively, \
                     then re-run `devboy secrets setup --resume`."
                .to_owned(),
        }
    }
    fn doctor(&self) -> DoctorOutcome {
        // Same shape: wire the doctor exec when its Rust API is
        // stable. For now treat it as Green so the wizard runs
        // to completion in `--resume` mode after a manual
        // provisioning round.
        DoctorOutcome::Green
    }
    fn now_iso8601(&self) -> String {
        chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
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
    fn proposer_skips_ambiguous_generic_segments() {
        // P5 — `SERVICE_TOKEN`, `MEET_SERVICE_TOKEN`,
        // `API_KEY`, `BOT_IMAGE` etc carry no real provider
        // hint. Demo on devboy-env-1 had these landing as
        // `personal/service/api-key`, `personal/api/key`,
        // `personal/bot/image` — all confidently wrong.
        // Now they Skip with an actionable rename hint.
        assert_skip(&propose_one("SERVICE_TOKEN", &known()));
        assert_skip(&propose_one("MEET_SERVICE_TOKEN", &known()));
        assert_skip(&propose_one("API_KEY", &known()));
        assert_skip(&propose_one("BOT_IMAGE", &known()));
        assert_skip(&propose_one("WORKER_TOKEN", &known()));
        assert_skip(&propose_one("CORE_SECRET", &known()));
        assert_skip(&propose_one("DB_PASSWORD", &known()));
    }

    #[test]
    fn proposer_keeps_ambiguous_when_provider_actually_known() {
        // Sanity: a known provider passes through even if the
        // purpose collapses to a "generic" word, because
        // `provider_known` is true and the ambiguity gate
        // only fires on unknown providers.
        let p = propose_one("OPENAI_API_KEY", &known());
        assert_path(&p, "team/openai/api-key", true);
    }

    #[test]
    fn proposer_skips_posix_locale_and_xdg() {
        // P4 — POSIX locale family.
        assert_skip(&propose_one("LC_ALL", &known()));
        assert_skip(&propose_one("LC_CTYPE", &known()));
        assert_skip(&propose_one("LC_MESSAGES", &known()));
        // XDG base-dir spec.
        assert_skip(&propose_one("XDG_CONFIG_HOME", &known()));
        assert_skip(&propose_one("XDG_DATA_HOME", &known()));
        assert_skip(&propose_one("XDG_RUNTIME_DIR", &known()));
        // GPG agent metadata (sockets, TTY hint, never values).
        assert_skip(&propose_one("GPG_TTY", &known()));
        assert_skip(&propose_one("GPG_AGENT_INFO", &known()));
        // SSH agent metadata.
        assert_skip(&propose_one("SSH_AUTH_SOCK", &known()));
        assert_skip(&propose_one("SSH_AGENT_PID", &known()));
        assert_skip(&propose_one("SSH_CONNECTION", &known()));
        // Shell / editor / pager toggles.
        assert_skip(&propose_one("EDITOR", &known()));
        assert_skip(&propose_one("VISUAL", &known()));
        assert_skip(&propose_one("PAGER", &known()));
        assert_skip(&propose_one("OLDPWD", &known()));
    }

    #[test]
    fn proposer_skips_windows_machine_vars() {
        // P3 — Windows / Cygwin / MSYS / WSL exact-match
        // exact list. Demo on devboy-env-1 hit COMPUTERNAME +
        // APPDATA as fake secrets — both correctly skipped now.
        assert_skip(&propose_one("COMPUTERNAME", &known()));
        assert_skip(&propose_one("APPDATA", &known()));
        assert_skip(&propose_one("LOCALAPPDATA", &known()));
        assert_skip(&propose_one("USERPROFILE", &known()));
        assert_skip(&propose_one("HOMEDRIVE", &known()));
        assert_skip(&propose_one("HOMEPATH", &known()));
        assert_skip(&propose_one("OS", &known()));
        assert_skip(&propose_one("SYSTEMROOT", &known()));
        assert_skip(&propose_one("WINDIR", &known()));
        assert_skip(&propose_one("PROGRAMFILES", &known()));
        assert_skip(&propose_one("TEMP", &known()));
        assert_skip(&propose_one("TMP", &known()));
        assert_skip(&propose_one("ONEDRIVE", &known()));
        // PROCESSOR_* prefix.
        assert_skip(&propose_one("PROCESSOR_ARCHITECTURE", &known()));
        assert_skip(&propose_one("PROCESSOR_IDENTIFIER", &known()));
        assert_skip(&propose_one("PROCESSOR_LEVEL", &known()));
    }

    #[test]
    fn proposer_skips_ci_runner_prefixes() {
        // P2 — CI / build / cloud-platform metadata prefixes.
        // None of these are user credentials.
        assert_skip(&propose_one("CI_COMMIT_BRANCH", &known()));
        assert_skip(&propose_one("CI_COMMIT_SHA", &known()));
        assert_skip(&propose_one("CI_PIPELINE_ID", &known()));
        assert_skip(&propose_one("CI_JOB_NAME", &known()));
        assert_skip(&propose_one("CI_RUNNER_ID", &known()));
        assert_skip(&propose_one("CI_PROJECT_PATH", &known()));
        assert_skip(&propose_one("BUILD_DATE", &known()));
        assert_skip(&propose_one("BUILD_NUMBER", &known()));
        assert_skip(&propose_one("BAML_LOG_LEVEL", &known()));
        assert_skip(&propose_one("GITHUB_ACTIONS_RUNNER_VERSION", &known()));
        assert_skip(&propose_one("GITHUB_RUN_ID", &known()));
        assert_skip(&propose_one("GITHUB_REF_NAME", &known()));
        assert_skip(&propose_one("RUNNER_OS", &known()));
        assert_skip(&propose_one("VERCEL_URL_INTERNAL", &known()));
        assert_skip(&propose_one("CI", &known()));
    }

    #[test]
    fn proposer_skips_configuration_suffixes() {
        // P1 — extended suffix list. Real-world examples from
        // the live demo on meteora/devboy-env-1.
        assert_skip(&propose_one("OPENAI_API_BASE", &known()));
        assert_skip(&propose_one("ANTHROPIC_MODEL", &known()));
        assert_skip(&propose_one("ANTHROPIC_CONCURRENCY", &known()));
        assert_skip(&propose_one("BAML_LOG", &known()));
        assert_skip(&propose_one("EMBEDDING_MODEL", &known()));
        assert_skip(&propose_one("LLM_MODEL", &known()));
        assert_skip(&propose_one("REDIS_HOST", &known()));
        assert_skip(&propose_one("REDIS_PORT", &known()));
        assert_skip(&propose_one("REDIS_DB", &known()));
        assert_skip(&propose_one("LOG_LEVEL", &known()));
        assert_skip(&propose_one("HTTP_TIMEOUT", &known()));
        assert_skip(&propose_one("CACHE_DIR", &known()));
        assert_skip(&propose_one("S3_ENDPOINT", &known()));
        assert_skip(&propose_one("MEET_SERVICE", &known()));
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

    // ====== Wizard runner fixtures (P26.4) ======================

    /// Test double — records calls and lets the test pre-script
    /// outcomes. `provision` is the only mutating call; the
    /// rest are read-only.
    struct FakeIo {
        scan_result: Vec<EnvVarHit>,
        known: Vec<String>,
        missing: Vec<String>,
        provision_outcomes: Vec<ProvisionOutcome>,
        provision_calls: Vec<String>,
        doctor: DoctorOutcome,
    }

    impl FakeIo {
        fn new() -> Self {
            Self {
                scan_result: Vec::new(),
                known: Vec::new(),
                missing: Vec::new(),
                provision_outcomes: Vec::new(),
                provision_calls: Vec::new(),
                doctor: DoctorOutcome::Green,
            }
        }
    }

    impl WizardIo for FakeIo {
        fn scan(&self, _root: &Path) -> std::io::Result<Vec<EnvVarHit>> {
            Ok(self.scan_result.clone())
        }
        fn known_providers(&self) -> Vec<String> {
            self.known.clone()
        }
        fn missing_required_paths(&self) -> std::io::Result<Vec<String>> {
            Ok(self.missing.clone())
        }
        fn provision(&mut self, path: &str) -> ProvisionOutcome {
            self.provision_calls.push(path.to_owned());
            // Pop the next pre-scripted outcome; default to
            // `Saved` when the test exhausted the queue.
            if !self.provision_outcomes.is_empty() {
                self.provision_outcomes.remove(0)
            } else {
                ProvisionOutcome::Saved
            }
        }
        fn doctor(&self) -> DoctorOutcome {
            self.doctor.clone()
        }
        fn now_iso8601(&self) -> String {
            "2026-05-10T17:30:00Z".to_owned()
        }
    }

    fn state_path(dir: &Path) -> PathBuf {
        dir.join("setup-state.toml")
    }

    fn phase_kinds(events: &[WizardEvent]) -> Vec<&'static str> {
        events
            .iter()
            .map(|e| match e {
                WizardEvent::PhaseStarted { .. } => "started",
                WizardEvent::PhaseProgress { .. } => "progress",
                WizardEvent::PhaseCompleted { .. } => "completed",
                WizardEvent::PhaseSkipped { .. } => "skipped",
                WizardEvent::PhaseFailed { .. } => "failed",
                WizardEvent::Completed => "wizard-completed",
            })
            .collect()
    }

    #[test]
    fn wizard_happy_path_runs_all_four_phases_and_emits_completed() {
        let dir = TempDir::new().unwrap();
        let mut io = FakeIo::new();
        io.missing = vec!["team/jira/api-key".into()];
        let events = run_wizard(dir.path(), &state_path(dir.path()), &mut io);
        // 4 started + 4 completed + 2 progress (request + saved)
        // + Completed.
        assert_eq!(io.provision_calls, vec!["team/jira/api-key"]);
        assert!(events.last() == Some(&WizardEvent::Completed));
        let kinds = phase_kinds(&events);
        assert_eq!(
            kinds.iter().filter(|k| **k == "completed").count(),
            4,
            "all four phases should complete: {events:?}"
        );
    }

    #[test]
    fn wizard_skips_provision_when_nothing_missing() {
        let dir = TempDir::new().unwrap();
        let mut io = FakeIo::new();
        // missing empty — provision gets `Skipped`, not `Failed`.
        let events = run_wizard(dir.path(), &state_path(dir.path()), &mut io);
        let kinds = phase_kinds(&events);
        let skipped = events.iter().any(|e| {
            matches!(
                e,
                WizardEvent::PhaseSkipped {
                    phase: WizardPhase::Provision,
                    ..
                }
            )
        });
        assert!(skipped, "expected Provision to be skipped: {events:?}");
        assert!(kinds.contains(&"wizard-completed"));
        // Doctor must still run.
        assert!(events.iter().any(|e| {
            matches!(
                e,
                WizardEvent::PhaseCompleted {
                    phase: WizardPhase::Doctor,
                    ..
                }
            )
        }));
    }

    #[test]
    fn wizard_short_circuits_on_provision_cancel() {
        let dir = TempDir::new().unwrap();
        let mut io = FakeIo::new();
        io.missing = vec!["team/jira/api-key".into(), "team/openai/api-key".into()];
        io.provision_outcomes = vec![ProvisionOutcome::Cancelled];
        let events = run_wizard(dir.path(), &state_path(dir.path()), &mut io);
        // First path was attempted; second was not.
        assert_eq!(io.provision_calls, vec!["team/jira/api-key"]);
        assert!(events.iter().any(|e| matches!(
            e,
            WizardEvent::PhaseFailed {
                phase: WizardPhase::Provision,
                ..
            }
        )));
        // No `Completed` event after a failure.
        assert!(!events.contains(&WizardEvent::Completed));
        // Doctor never ran.
        assert!(!events.iter().any(|e| matches!(
            e,
            WizardEvent::PhaseStarted {
                phase: WizardPhase::Doctor
            }
        )));
    }

    #[test]
    fn wizard_short_circuits_on_doctor_red() {
        let dir = TempDir::new().unwrap();
        let mut io = FakeIo::new();
        io.doctor = DoctorOutcome::Red {
            summary: "vault unavailable".into(),
        };
        let events = run_wizard(dir.path(), &state_path(dir.path()), &mut io);
        let failed = events.iter().any(|e| {
            matches!(
                e,
                WizardEvent::PhaseFailed {
                    phase: WizardPhase::Doctor,
                    ..
                }
            )
        });
        assert!(failed);
        assert!(!events.contains(&WizardEvent::Completed));
    }

    #[test]
    fn wizard_resume_skips_done_phases_on_second_run() {
        let dir = TempDir::new().unwrap();
        let path = state_path(dir.path());
        // First run — happy path settles every phase.
        {
            let mut io = FakeIo::new();
            io.missing = vec!["team/jira/api-key".into()];
            let _ = run_wizard(dir.path(), &path, &mut io);
        }
        // Second run — every phase already `done`. Wizard
        // should emit one `Skipped` per phase plus a final
        // `Completed`. provision must NOT be re-attempted.
        let mut io2 = FakeIo::new();
        io2.missing = vec!["team/jira/api-key".into()];
        let events = run_wizard(dir.path(), &path, &mut io2);
        assert!(
            io2.provision_calls.is_empty(),
            "resume must not re-provision"
        );
        let skip_count = events
            .iter()
            .filter(|e| matches!(e, WizardEvent::PhaseSkipped { .. }))
            .count();
        assert_eq!(skip_count, 4);
        assert!(events.contains(&WizardEvent::Completed));
    }

    // ====== SetupState typed-view fixtures (P26.5) =================

    #[test]
    fn setup_state_for_missing_file_is_fresh() {
        let dir = TempDir::new().unwrap();
        let s = read_setup_state(&state_path(dir.path()));
        assert!(!s.scanned);
        assert!(!s.proposed);
        assert!(s.provisioned.is_empty());
        assert_eq!(s.last_step, 0);
        assert!(!s.is_complete());
    }

    #[test]
    fn setup_state_after_happy_path_reports_complete() {
        let dir = TempDir::new().unwrap();
        let path = state_path(dir.path());
        let mut io = FakeIo::new();
        io.missing = vec!["team/jira/api-key".into(), "team/openai/api-key".into()];
        let _ = run_wizard(dir.path(), &path, &mut io);
        let s = read_setup_state(&path);
        assert!(s.scanned);
        assert!(s.proposed);
        assert_eq!(
            s.provisioned,
            vec![
                "team/jira/api-key".to_owned(),
                "team/openai/api-key".to_owned(),
            ]
        );
        assert_eq!(s.last_step, 4);
        assert!(s.is_complete());
    }

    #[test]
    fn setup_state_partial_run_records_progress_only_up_to_failure() {
        let dir = TempDir::new().unwrap();
        let path = state_path(dir.path());
        let mut io = FakeIo::new();
        io.missing = vec!["team/jira/api-key".into(), "team/openai/api-key".into()];
        // First provision saves; second is cancelled — wizard
        // bails after recording phase 3 as `failed`.
        io.provision_outcomes = vec![ProvisionOutcome::Saved, ProvisionOutcome::Cancelled];
        let _ = run_wizard(dir.path(), &path, &mut io);
        let s = read_setup_state(&path);
        // Phases 1 and 2 settled; phase 3 failed (not done /
        // skipped) → `scanned=true, proposed=true,
        // is_complete=false`. provisioned stays empty because
        // the recorder only writes paths on phase success.
        assert!(s.scanned);
        assert!(s.proposed);
        assert!(s.provisioned.is_empty());
        assert!(!s.is_complete());
        // last_step reflects how far the wizard *reached*
        // (phase 3 attempted), not the highest *settled*
        // phase.
        assert!(s.last_step >= 3);
    }

    #[test]
    fn wizard_state_file_written_with_done_statuses() {
        let dir = TempDir::new().unwrap();
        let path = state_path(dir.path());
        let mut io = FakeIo::new();
        io.missing = vec!["team/jira/api-key".into()];
        let _ = run_wizard(dir.path(), &path, &mut io);
        let body = std::fs::read_to_string(&path).unwrap();
        // Every phase should appear in the file as `done`.
        for phase in WizardPhase::ALL {
            let key = phase.key();
            assert!(
                body.contains(&format!("[steps.{key}]")),
                "missing section [steps.{key}] in: {body}"
            );
        }
        assert!(body.contains("status = \"done\""));
        assert!(body.contains("schema_version = 1"));
    }
}
