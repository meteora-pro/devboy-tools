//! Integration tests for the `devboy secrets catalog ...`
//! subcommands added in this epic: `status`, `add-url`,
//! `refresh`, `forget`, `pin`. Each test spawns the `devboy`
//! binary with `HOME` redirected to a fresh tempdir so the
//! catalog state (`sources.toml`, `known_hashes.toml`, cache,
//! audit log) lives entirely inside the test sandbox and is
//! discarded on drop.
//!
//! What the file covers:
//!
//! - happy path of `catalog status --json` against the bundled
//!   catalogs (no network, no on-disk URL sources);
//! - guard rejections in `catalog add-url` (non-https URL,
//!   loopback SSRF target) — both must fail before any state
//!   file is touched;
//! - lifecycle of `catalog forget`: empty state, no-match
//!   filter, scoped drop, bulk drop with `--yes`;
//! - lifecycle of `catalog pin`: missing sources.toml,
//!   explicit-sha pin, implicit copy from a recorded TOFU
//!   entry, ambiguity error, bad-sha refusal;
//! - `catalog refresh` edge cases (no sources.toml, empty
//!   sources.toml).
//!
//! What this file deliberately does NOT cover:
//!
//! - End-to-end add-url against a real HTTPS catalog. The
//!   SSRF guard refuses loopback so httpmock cannot stand in;
//!   the catalog crate's own tests cover the fetch protocol
//!   via `fetch_no_https_guard`.

#![cfg(not(target_os = "windows"))]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

// ---------------------------------------------------------------
// Test harness
// ---------------------------------------------------------------

fn devboy_bin() -> PathBuf {
    let mut path = std::env::current_exe().unwrap();
    path.pop(); // remove test binary name
    path.pop(); // remove `deps/`
    path.push(format!("devboy{}", std::env::consts::EXE_SUFFIX));
    path
}

/// Hermetic HOME for one test. The catalog code paths all
/// resolve `~/.devboy/secrets/catalog/...` via
/// `dirs::home_dir()`, which reads `$HOME` on Unix-likes — so
/// pointing HOME at a tempdir is enough to scope all
/// filesystem reads + writes to the sandbox.
struct CatalogEnv {
    home: TempDir,
}

impl CatalogEnv {
    fn new() -> Self {
        Self {
            home: TempDir::new().unwrap(),
        }
    }

    fn home_path(&self) -> &Path {
        self.home.path()
    }

    fn catalog_dir(&self) -> PathBuf {
        self.home_path()
            .join(".devboy")
            .join("secrets")
            .join("catalog")
    }

    fn sources_toml_path(&self) -> PathBuf {
        self.catalog_dir().join("sources.toml")
    }

    fn known_hashes_path(&self) -> PathBuf {
        self.catalog_dir().join("known_hashes.toml")
    }

    fn write_sources_toml(&self, body: &str) {
        let path = self.sources_toml_path();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, body).unwrap();
    }

    fn write_known_hashes(&self, body: &str) {
        let path = self.known_hashes_path();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, body).unwrap();
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(devboy_bin());
        c.env_clear();
        c.env("HOME", self.home_path());
        c.env("XDG_CONFIG_HOME", self.home_path().join(".config"));
        if let Ok(path_var) = std::env::var("PATH") {
            c.env("PATH", path_var);
        }
        c.args(args);
        c
    }
}

fn run_ok(env: &CatalogEnv, args: &[&str]) -> String {
    let out = env.cmd(args).output().expect("spawn devboy");
    assert!(
        out.status.success(),
        "expected `devboy {}` to succeed, stderr=\n{}\nstdout=\n{}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout),
    );
    String::from_utf8(out.stdout).expect("stdout is utf-8")
}

fn run_fail(env: &CatalogEnv, args: &[&str]) -> (String, String) {
    let out = env.cmd(args).output().expect("spawn devboy");
    assert!(
        !out.status.success(),
        "expected `devboy {}` to fail, stdout=\n{}\nstderr=\n{}",
        args.join(" "),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    (
        String::from_utf8(out.stdout).expect("stdout is utf-8"),
        String::from_utf8(out.stderr).expect("stderr is utf-8"),
    )
}

// ---------------------------------------------------------------
// `catalog status`
// ---------------------------------------------------------------

#[test]
fn status_json_lists_bundled_catalogs_by_default() {
    // Nothing on disk → only the bundled catalogs load. The
    // count must be > 0 and the JSON shape must include
    // `loaded`, `errors`, `catalogs`.
    let env = CatalogEnv::new();
    let stdout = run_ok(&env, &["secrets", "catalog", "status", "--json"]);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("status --json is JSON");

    let loaded = parsed["loaded"].as_u64().expect("loaded is u64");
    assert!(loaded >= 8, "expected ≥8 bundled catalogs, got {loaded}");
    assert_eq!(
        parsed["errors"].as_array().expect("errors array").len(),
        0,
        "bundled-only setup must load without errors:\n{parsed}"
    );
    let catalogs = parsed["catalogs"].as_array().expect("catalogs array");
    assert!(
        catalogs.iter().all(|c| c["origin"] == "bundled"),
        "no on-disk catalogs → every entry must report origin=bundled"
    );
    // Every bundled catalog must carry a provider_id + a
    // non-zero variants count (otherwise the proposer has no
    // patterns to match on).
    for c in catalogs {
        assert!(c["provider_id"].is_string(), "missing provider_id: {c}");
        assert!(
            c["variants"].as_u64().unwrap_or(0) >= 1,
            "bundled catalog has zero variants: {c}"
        );
    }
}

#[test]
fn status_human_output_lists_header_row() {
    // The default (non-JSON) form prints a fixed-width table;
    // exercising it gives us coverage of the table-render
    // branch even though we don't pin every column position.
    let env = CatalogEnv::new();
    let stdout = run_ok(&env, &["secrets", "catalog", "status"]);
    assert!(
        stdout.contains("provider") && stdout.contains("origin") && stdout.contains("variants"),
        "expected header row with provider/origin/variants, got:\n{stdout}"
    );
}

// ---------------------------------------------------------------
// `catalog add-url` — guard rejections
// ---------------------------------------------------------------

#[test]
fn add_url_rejects_non_https_scheme() {
    let env = CatalogEnv::new();
    let (_stdout, stderr) = run_fail(
        &env,
        &[
            "secrets",
            "catalog",
            "add-url",
            "http://example.invalid/catalog.json",
            "--yes",
        ],
    );
    assert!(
        stderr.contains("https://"),
        "stderr must explain the https:// requirement, got:\n{stderr}"
    );
    // Neither sources.toml nor known_hashes.toml may exist —
    // the bail happens before any FS write.
    assert!(
        !env.sources_toml_path().exists(),
        "sources.toml must not be created on guard rejection"
    );
    assert!(
        !env.known_hashes_path().exists(),
        "known_hashes.toml must not be created on guard rejection"
    );
}

#[test]
fn add_url_rejects_loopback_target_via_ssrf_guard() {
    // 127.0.0.1 is loopback; the SSRF guard refuses it. We
    // pin sha so the loader doesn't ask for confirmation
    // first — the guard runs before any pin check.
    let env = CatalogEnv::new();
    let (_stdout, stderr) = run_fail(
        &env,
        &[
            "secrets",
            "catalog",
            "add-url",
            "https://127.0.0.1/catalog.json",
            "--pin",
            &"a".repeat(64),
        ],
    );
    let blob = stderr.to_lowercase();
    assert!(
        blob.contains("ssrf") || blob.contains("loopback") || blob.contains("refused"),
        "stderr must mention the SSRF guard, got:\n{stderr}"
    );
}

// ---------------------------------------------------------------
// `catalog forget`
// ---------------------------------------------------------------

#[test]
fn forget_reports_empty_when_no_known_hashes_file() {
    // No known_hashes.toml at all → command emits a single
    // "nothing to forget" line and exits clean.
    let env = CatalogEnv::new();
    let stdout = run_ok(&env, &["secrets", "catalog", "forget"]);
    assert!(
        stdout.contains("empty") && stdout.contains("nothing to forget"),
        "expected 'empty … nothing to forget', got:\n{stdout}"
    );
}

#[test]
fn forget_drops_only_matching_url_when_filter_given() {
    let env = CatalogEnv::new();
    env.write_known_hashes(
        r#"[url]
"https://catalog.example.com/anthropic.json" = "11111111111111111111111111111111111111111111111111111111111111aa"
"https://catalog.example.com/openai.json" = "22222222222222222222222222222222222222222222222222222222222222bb"
"#,
    );

    let stdout = run_ok(&env, &["secrets", "catalog", "forget", "openai"]);
    assert!(
        stdout.contains("forget https://catalog.example.com/openai.json"),
        "stdout must name the dropped URL:\n{stdout}"
    );
    assert!(
        !stdout.contains("anthropic.json"),
        "filter must spare the non-matching URL:\n{stdout}"
    );

    let after = fs::read_to_string(env.known_hashes_path()).unwrap();
    assert!(
        after.contains("anthropic.json"),
        "anthropic entry must survive:\n{after}"
    );
    assert!(
        !after.contains("openai.json"),
        "openai entry must be gone:\n{after}"
    );
}

#[test]
fn forget_no_match_reports_no_change() {
    let env = CatalogEnv::new();
    env.write_known_hashes(
        r#"[url]
"https://catalog.example.com/anthropic.json" = "11111111111111111111111111111111111111111111111111111111111111aa"
"#,
    );
    let stdout = run_ok(&env, &["secrets", "catalog", "forget", "no-such-url"]);
    assert!(
        stdout.contains("no URLs in known_hashes.toml match"),
        "expected no-match notice, got:\n{stdout}"
    );
    // Source file untouched.
    let after = fs::read_to_string(env.known_hashes_path()).unwrap();
    assert!(
        after.contains("anthropic.json"),
        "file must be unchanged:\n{after}"
    );
}

#[test]
fn forget_bulk_drops_every_entry_with_yes_flag() {
    let env = CatalogEnv::new();
    env.write_known_hashes(
        r#"[url]
"https://catalog.example.com/anthropic.json" = "11111111111111111111111111111111111111111111111111111111111111aa"
"https://catalog.example.com/openai.json" = "22222222222222222222222222222222222222222222222222222222222222bb"
"#,
    );
    let stdout = run_ok(&env, &["secrets", "catalog", "forget", "--yes"]);
    assert!(
        stdout.contains("2 entries dropped"),
        "expected '2 entries dropped', got:\n{stdout}"
    );
    let after = fs::read_to_string(env.known_hashes_path()).unwrap();
    assert!(
        !after.contains("anthropic.json") && !after.contains("openai.json"),
        "all entries must be gone:\n{after}"
    );
}

#[test]
fn forget_bulk_without_yes_refuses_under_non_tty() {
    let env = CatalogEnv::new();
    env.write_known_hashes(
        r#"[url]
"https://catalog.example.com/anthropic.json" = "11111111111111111111111111111111111111111111111111111111111111aa"
"#,
    );
    let (_stdout, stderr) = run_fail(&env, &["secrets", "catalog", "forget"]);
    assert!(
        stderr.contains("non-tty") || stderr.contains("--yes"),
        "stderr must explain the --yes requirement, got:\n{stderr}"
    );
    // Source file must be untouched.
    let after = fs::read_to_string(env.known_hashes_path()).unwrap();
    assert!(
        after.contains("anthropic.json"),
        "file must be unchanged:\n{after}"
    );
}

// ---------------------------------------------------------------
// `catalog pin`
// ---------------------------------------------------------------

const VALID_SHA_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const VALID_SHA_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

#[test]
fn pin_fails_when_no_sources_toml() {
    let env = CatalogEnv::new();
    let (_stdout, stderr) = run_fail(
        &env,
        &["secrets", "catalog", "pin", "anthropic", VALID_SHA_A],
    );
    assert!(
        stderr.contains("no sources.toml"),
        "stderr must explain the missing file, got:\n{stderr}"
    );
}

#[test]
fn pin_writes_explicit_sha_into_matching_source() {
    let env = CatalogEnv::new();
    env.write_sources_toml(
        r#"enable_url_catalogs = true

[[source]]
url = "https://catalog.example.com/anthropic.json"
refresh_seconds = 86400
"#,
    );
    let stdout = run_ok(
        &env,
        &["secrets", "catalog", "pin", "anthropic", VALID_SHA_A],
    );
    assert!(
        stdout.contains(&format!("sha256 = {VALID_SHA_A}")),
        "stdout must print the pinned sha, got:\n{stdout}"
    );
    let after = fs::read_to_string(env.sources_toml_path()).unwrap();
    assert!(
        after.contains(VALID_SHA_A),
        "sources.toml must carry the new pin:\n{after}"
    );
}

#[test]
fn pin_copies_tofu_sha_when_no_explicit_sha_given() {
    let env = CatalogEnv::new();
    env.write_sources_toml(
        r#"enable_url_catalogs = true

[[source]]
url = "https://catalog.example.com/anthropic.json"
refresh_seconds = 86400
"#,
    );
    env.write_known_hashes(&format!(
        r#"[url]
"https://catalog.example.com/anthropic.json" = "{VALID_SHA_B}"
"#,
    ));

    let stdout = run_ok(&env, &["secrets", "catalog", "pin", "anthropic"]);
    assert!(
        stdout.contains(&format!("sha256 = {VALID_SHA_B}")),
        "stdout must promote the TOFU sha, got:\n{stdout}"
    );
    let after = fs::read_to_string(env.sources_toml_path()).unwrap();
    assert!(
        after.contains(VALID_SHA_B),
        "sources.toml must carry the promoted TOFU sha:\n{after}"
    );
}

#[test]
fn pin_rejects_malformed_sha() {
    let env = CatalogEnv::new();
    env.write_sources_toml(
        r#"enable_url_catalogs = true

[[source]]
url = "https://catalog.example.com/anthropic.json"
refresh_seconds = 86400
"#,
    );
    let (_stdout, stderr) = run_fail(
        &env,
        &["secrets", "catalog", "pin", "anthropic", "not-a-sha"],
    );
    assert!(
        stderr.contains("64 lower-case hex chars"),
        "stderr must explain the sha format, got:\n{stderr}"
    );
}

#[test]
fn pin_filter_with_no_match_bails() {
    let env = CatalogEnv::new();
    env.write_sources_toml(
        r#"enable_url_catalogs = true

[[source]]
url = "https://catalog.example.com/anthropic.json"
refresh_seconds = 86400
"#,
    );
    let (_stdout, stderr) = run_fail(
        &env,
        &["secrets", "catalog", "pin", "no-such-url", VALID_SHA_A],
    );
    assert!(
        stderr.contains("no source URL contains"),
        "stderr must name the missing filter, got:\n{stderr}"
    );
}

#[test]
fn pin_ambiguous_filter_lists_the_matches() {
    let env = CatalogEnv::new();
    env.write_sources_toml(
        r#"enable_url_catalogs = true

[[source]]
url = "https://catalog.example.com/anthropic.json"

[[source]]
url = "https://catalog.example.com/anthropic-beta.json"
"#,
    );
    let (_stdout, stderr) = run_fail(
        &env,
        &["secrets", "catalog", "pin", "anthropic", VALID_SHA_A],
    );
    assert!(
        stderr.contains("matches 2 sources"),
        "stderr must report the ambiguity, got:\n{stderr}"
    );
    assert!(
        stderr.contains("anthropic.json") && stderr.contains("anthropic-beta.json"),
        "stderr must list both URLs, got:\n{stderr}"
    );
}

#[test]
fn pin_without_tofu_entry_directs_user_to_refresh() {
    let env = CatalogEnv::new();
    env.write_sources_toml(
        r#"enable_url_catalogs = true

[[source]]
url = "https://catalog.example.com/anthropic.json"
"#,
    );
    // No known_hashes.toml on disk.
    let (_stdout, stderr) = run_fail(&env, &["secrets", "catalog", "pin", "anthropic"]);
    assert!(
        stderr.contains("no TOFU entry recorded"),
        "stderr must direct the user to `catalog refresh`, got:\n{stderr}"
    );
}

// ---------------------------------------------------------------
// `catalog refresh`
// ---------------------------------------------------------------

#[test]
fn refresh_reports_no_file_when_sources_toml_missing() {
    let env = CatalogEnv::new();
    let stdout = run_ok(&env, &["secrets", "catalog", "refresh"]);
    assert!(
        stdout.contains("no sources.toml") && stdout.contains("nothing to refresh"),
        "expected no-file notice, got:\n{stdout}"
    );
}

#[test]
fn refresh_reports_empty_when_no_source_entries() {
    let env = CatalogEnv::new();
    env.write_sources_toml("enable_url_catalogs = true\n");
    let stdout = run_ok(&env, &["secrets", "catalog", "refresh"]);
    assert!(
        stdout.contains("no [[source]] entries"),
        "expected empty-entries notice, got:\n{stdout}"
    );
}

#[test]
fn refresh_with_filter_skipping_every_source_reports_clean_run() {
    // Two sources, filter matches neither — refresh skips
    // both and exits clean. The fetch path never runs, so we
    // dodge the loopback/SSRF problem entirely.
    let env = CatalogEnv::new();
    env.write_sources_toml(
        r#"enable_url_catalogs = true

[[source]]
url = "https://catalog.example.com/anthropic.json"

[[source]]
url = "https://catalog.example.com/openai.json"
"#,
    );
    let stdout = run_ok(
        &env,
        &["secrets", "catalog", "refresh", "no-such-substring"],
    );
    assert!(
        stdout.contains("0 fetched, 2 skipped (filter), 0 failed"),
        "expected '0 fetched, 2 skipped, 0 failed', got:\n{stdout}"
    );
}
