//! `devboy hooks` — install + run git hooks for the secret
//! framework per [ADR-020] §5 last paragraph.
//!
//! ADR-020 §5:
//!
//! > A pre-commit hook (`devboy hooks install
//! > --secret-alias-lint`) is provided as a defensive measure: if
//! > an unresolved `@secret:<path>` ends up committed in a file
//! > that is **not** known to be alias-aware (for example, an
//! > arbitrary `.env` file that is not loaded through the
//! > `devboy-tools` config loader), the hook fails the commit and
//! > prints the offending file and line.
//!
//! Two subcommands:
//!
//! - `devboy hooks install --secret-alias-lint [--force]` —
//!   writes a tiny `pre-commit` shell script that calls back into
//!   `devboy hooks check secret-alias`. Refuses to clobber an
//!   existing hook without `--force`.
//! - `devboy hooks check secret-alias` — scans the *staged* git
//!   diff (`git diff --cached`) for `@secret:<path>` aliases in
//!   files that aren't known to be alias-aware. Exit code 0 on
//!   clean, non-zero with a per-line diagnostic on hits.
//!
//! ## Alias-aware file classification
//!
//! "Alias-aware" means: this file is parsed by code that resolves
//! aliases at use-time (the devboy config loader, P8.1's
//! `SecretResolver`). Aliases there are expected. The hook
//! tolerates them.
//!
//! "Suspicious" means: this file holds raw configuration the
//! devboy loader does not touch. An alias in there is dead text:
//! the consumer of the file will see the literal `@secret:...`
//! string and either crash or — worse — *use* it as the secret.
//! The hook flags it.
//!
//! Default classification:
//!
//! | Path                         | Class           |
//! |------------------------------|-----------------|
//! | `.devboy/**` / `.devboy.toml`| AliasAware      |
//! | `*.md` / `*.rst`             | AliasAware (docs may show example aliases) |
//! | `Cargo.toml`                 | AliasAware (workspace metadata)    |
//! | `.env*`                      | Suspicious      |
//! | `*.yaml` / `*.yml`           | Suspicious      |
//! | `*.json`                     | Suspicious      |
//! | `*.toml` (other)             | Suspicious      |
//! | `*.sh` / `*.bash`            | Suspicious      |
//! | anything else                | Unknown (skip)  |
//!
//! [ADR-020]: https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-020-secret-manifest-and-alias-resolution.md

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use devboy_core::alias::ALIAS_PREFIX;

/// `devboy hooks <subcommand>` family.
#[derive(Subcommand, Debug)]
pub enum HooksCommands {
    /// Install git hooks. Currently only the secret-alias-lint
    /// pre-commit hook is supported; future hooks land as
    /// additional flags on the same subcommand.
    Install(InstallArgs),
    /// Run a hook check directly. Used by the installed hooks but
    /// also as a standalone way to lint the staged diff before
    /// committing.
    Check {
        /// Which check to run.
        #[command(subcommand)]
        command: CheckCommands,
    },
}

/// `devboy hooks check <which>`.
#[derive(Subcommand, Debug)]
pub enum CheckCommands {
    /// Scan the staged git diff for `@secret:<path>` aliases in
    /// files not known to be alias-aware. Exits non-zero on hits.
    SecretAlias(SecretAliasArgs),
}

/// Flags for `devboy hooks install`.
#[derive(Args, Debug, Default)]
pub struct InstallArgs {
    /// Install the secret-alias pre-commit lint. Currently the
    /// only thing `install` knows about; the flag is required so
    /// future hooks can be added without changing the verb.
    #[arg(long)]
    pub secret_alias_lint: bool,
    /// Replace an existing `pre-commit` hook. Without this the
    /// install refuses when the hook file already exists.
    #[arg(short, long, default_value_t = false)]
    pub force: bool,
}

/// Flags for `devboy hooks check secret-alias`.
#[derive(Args, Debug, Default)]
pub struct SecretAliasArgs {
    /// Override the working directory the check runs in. Mostly
    /// useful for tests; defaults to the current process CWD.
    #[arg(long)]
    pub repo: Option<PathBuf>,
}

// =============================================================================
// Dispatch
// =============================================================================

pub fn handle(command: HooksCommands) -> Result<()> {
    match command {
        HooksCommands::Install(args) => install(args),
        HooksCommands::Check { command } => match command {
            CheckCommands::SecretAlias(args) => run_secret_alias_check(args.repo.as_deref()),
        },
    }
}

// =============================================================================
// install
// =============================================================================

const PRE_COMMIT_SCRIPT: &str = "\
#!/bin/sh
# Installed by `devboy hooks install --secret-alias-lint`.
# Fails the commit when an unresolved @secret:<path> alias appears
# in a file not known to be alias-aware. See ADR-020 §5.
exec devboy hooks check secret-alias
";

fn install(args: InstallArgs) -> Result<()> {
    if !args.secret_alias_lint {
        bail!(
            "no hook selected; pass --secret-alias-lint to install the secret-alias \
             pre-commit lint"
        );
    }

    let repo_root = git_top_level(None)
        .context("could not determine git repository root (run from inside a git repo)")?;
    let hook_path = repo_root.join(".git").join("hooks").join("pre-commit");
    write_hook_file(&hook_path, PRE_COMMIT_SCRIPT, args.force)?;

    println!(
        "installed pre-commit hook at {} (runs `devboy hooks check secret-alias`)",
        hook_path.display()
    );
    Ok(())
}

fn write_hook_file(path: &Path, body: &str, force: bool) -> Result<()> {
    if path.exists() && !force {
        bail!(
            "{} already exists; pass --force to overwrite",
            path.display()
        );
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    std::fs::write(path, body).with_context(|| format!("failed to write {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(path)?.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(path, perms)
            .with_context(|| format!("failed to chmod +x {}", path.display()))?;
    }
    Ok(())
}

fn git_top_level(cwd: Option<&Path>) -> Result<PathBuf> {
    let mut cmd = Command::new("git");
    cmd.args(["rev-parse", "--show-toplevel"]);
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    let out = cmd
        .output()
        .with_context(|| "failed to invoke `git`; is it on PATH?")?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        bail!("git rev-parse --show-toplevel: {}", stderr.trim());
    }
    let raw = String::from_utf8(out.stdout).context("git output was not UTF-8")?;
    Ok(PathBuf::from(raw.trim_end_matches('\n')))
}

// =============================================================================
// check secret-alias
// =============================================================================

/// One alias-in-suspicious-file finding from the lint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AliasFinding {
    /// Path of the file (relative to repo root).
    pub file: String,
    /// 1-indexed line number in the *new* version of the file.
    pub new_line: usize,
    /// The full added line as the user wrote it.
    pub line: String,
    /// Path portion of the alias (the part after `@secret:`).
    pub alias_path: String,
}

fn run_secret_alias_check(repo: Option<&Path>) -> Result<()> {
    let staged_files = staged_files(repo)?;

    let mut findings = Vec::new();
    for (mode, file) in &staged_files {
        // Skip deleted files — nothing to scan.
        if mode == "D" {
            continue;
        }
        if classify_file(file) != FileClass::Suspicious {
            continue;
        }
        let patch = staged_patch(repo, file)?;
        findings.extend(find_aliases_in_added_lines(file, &patch));
    }

    if findings.is_empty() {
        return Ok(());
    }

    eprintln!(
        "secret-alias-lint: found {} unresolved `@secret:<path>` alias(es) in files \
         not known to be alias-aware:",
        findings.len()
    );
    for f in &findings {
        eprintln!("  {}:{}: {}", f.file, f.new_line, f.line.trim());
        eprintln!("    → alias path: {}", f.alias_path);
    }
    eprintln!();
    eprintln!(
        "Aliases are only resolved at use-time by code that knows about devboy-tools. \
         A literal @secret:<path> in a non-aware file (.env, plain YAML, …) will be \
         consumed verbatim by its reader, which is almost certainly not what you want."
    );
    bail!("aborting commit");
}

fn staged_files(cwd: Option<&Path>) -> Result<Vec<(String, String)>> {
    // `--diff-filter=AM` covers Added and Modified — we don't lint
    // pure Deletes (no content to scan) or Renames (Git emits
    // these as paired entries, and the *content* didn't change).
    let mut cmd = Command::new("git");
    cmd.args([
        "diff",
        "--cached",
        "--name-status",
        "--diff-filter=AM",
        "-z",
    ]);
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    let out = cmd.output().with_context(|| "failed to invoke `git`")?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        bail!(
            "git diff --cached --name-status -z exited with {}: {}",
            out.status,
            stderr.trim()
        );
    }
    let raw = String::from_utf8(out.stdout).context("git output was not UTF-8")?;
    let mut entries = Vec::new();
    // With -z, `--name-status` separates *every* field by NUL —
    // status, path, status, path, …. Renames would emit a 3-tuple
    // (`Rxxx`, src, dst), but `--diff-filter=AM` excludes those.
    let mut parts = raw.split('\0').filter(|s| !s.is_empty());
    while let Some(status) = parts.next() {
        let Some(path) = parts.next() else { break };
        entries.push((status.to_owned(), path.to_owned()));
    }
    Ok(entries)
}

fn staged_patch(cwd: Option<&Path>, file: &str) -> Result<String> {
    let mut cmd = Command::new("git");
    cmd.args(["diff", "--cached", "-U0", "--", file]);
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    let out = cmd.output().with_context(|| "failed to invoke `git`")?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        bail!(
            "git diff --cached -U0 -- {file} exited with {}: {}",
            out.status,
            stderr.trim()
        );
    }
    String::from_utf8(out.stdout).context("git diff output was not UTF-8")
}

// =============================================================================
// File classification + diff parsing (pure helpers)
// =============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileClass {
    /// Aliases are expected here. Skip.
    AliasAware,
    /// Aliases here are almost certainly a mistake. Lint.
    Suspicious,
    /// Default — out of scope for the lint.
    Unknown,
}

/// Classify a repo-relative path. Used by the lint and by
/// downstream tools that want to mirror the same rules.
pub fn classify_file(path: &str) -> FileClass {
    let lower = path.to_ascii_lowercase();

    // 1) Project's own devboy config — definitely alias-aware.
    if path == ".devboy.toml"
        || path.starts_with(".devboy/")
        || path.contains("/.devboy/")
        || path.ends_with("/.devboy.toml")
    {
        return FileClass::AliasAware;
    }

    // 2) Documentation files — examples may include aliases.
    if lower.ends_with(".md") || lower.ends_with(".rst") || lower.ends_with(".markdown") {
        return FileClass::AliasAware;
    }

    // 3) Workspace / package metadata.
    if path.ends_with("Cargo.toml") {
        return FileClass::AliasAware;
    }

    // 4) Suspicious file shapes — formats we know consumers will
    //    read literally, so an alias in them won't be resolved.
    let basename = Path::new(path)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(path);
    if basename == ".env" || basename.starts_with(".env.") {
        return FileClass::Suspicious;
    }
    for ext in &[".yaml", ".yml", ".json", ".toml", ".sh", ".bash"] {
        if lower.ends_with(ext) {
            return FileClass::Suspicious;
        }
    }

    FileClass::Unknown
}

/// Find every `@secret:<path>` alias on an *added* line of a
/// unified-diff `patch`. Lines starting with `+` (but not `+++`)
/// are added; the function tracks the new-side line number from
/// `@@` hunk headers so findings carry useful coordinates.
pub fn find_aliases_in_added_lines(file: &str, patch: &str) -> Vec<AliasFinding> {
    let mut findings = Vec::new();
    let mut new_line: usize = 0;

    for raw in patch.lines() {
        if let Some(rest) = raw.strip_prefix("@@") {
            // Header: `@@ -a,b +c,d @@ ...`. Reset `new_line` to
            // `c - 1` so the first incremented `+`-line is `c`.
            if let Some(start) = parse_new_hunk_start(rest) {
                new_line = start.saturating_sub(1);
            }
            continue;
        }
        if raw.starts_with("+++ ") || raw.starts_with("--- ") || raw.starts_with("\\ No newline") {
            continue;
        }
        if let Some(content) = raw.strip_prefix('+') {
            new_line += 1;
            if let Some(alias_path) = extract_alias_path(content) {
                findings.push(AliasFinding {
                    file: file.to_owned(),
                    new_line,
                    line: content.to_owned(),
                    alias_path,
                });
            }
        } else if raw.starts_with(' ') {
            // Context line — count against new side.
            new_line += 1;
        }
        // '-' lines and unknown prefixes don't increment new_line.
    }

    findings
}

fn parse_new_hunk_start(rest: &str) -> Option<usize> {
    // rest looks like ` -a,b +c,d @@ ...`. Pull out `+c`.
    let plus = rest.find('+')?;
    let tail = &rest[plus + 1..];
    let end = tail
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(tail.len());
    tail[..end].parse().ok()
}

fn extract_alias_path(line: &str) -> Option<String> {
    let idx = line.find(ALIAS_PREFIX)?;
    let tail = &line[idx + ALIAS_PREFIX.len()..];
    // Take alias-path characters: lowercase letters / digits /
    // `/` / `-` / `_`. Per ADR-020 path validation; stops at the
    // first character outside that set.
    let end = tail
        .find(|c: char| {
            !(c.is_ascii_lowercase() || c.is_ascii_digit() || c == '/' || c == '-' || c == '_')
        })
        .unwrap_or(tail.len());
    if end == 0 {
        return None;
    }
    Some(tail[..end].to_owned())
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // -- classify_file -------------------------------------------

    #[test]
    fn classifies_devboy_config_as_alias_aware() {
        assert_eq!(classify_file(".devboy.toml"), FileClass::AliasAware);
        assert_eq!(classify_file(".devboy/secrets.toml"), FileClass::AliasAware);
        assert_eq!(classify_file("subdir/.devboy.toml"), FileClass::AliasAware);
        assert_eq!(
            classify_file("subdir/.devboy/contexts.toml"),
            FileClass::AliasAware
        );
    }

    #[test]
    fn classifies_docs_as_alias_aware() {
        assert_eq!(classify_file("README.md"), FileClass::AliasAware);
        assert_eq!(classify_file("docs/onboarding.md"), FileClass::AliasAware);
        assert_eq!(classify_file("PYINSTALL.rst"), FileClass::AliasAware);
    }

    #[test]
    fn classifies_cargo_manifests_as_alias_aware() {
        assert_eq!(classify_file("Cargo.toml"), FileClass::AliasAware);
        assert_eq!(
            classify_file("crates/foo/Cargo.toml"),
            FileClass::AliasAware
        );
    }

    #[test]
    fn classifies_env_files_as_suspicious() {
        assert_eq!(classify_file(".env"), FileClass::Suspicious);
        assert_eq!(classify_file(".env.production"), FileClass::Suspicious);
        assert_eq!(classify_file(".env.local"), FileClass::Suspicious);
        assert_eq!(classify_file("backend/.env"), FileClass::Suspicious);
    }

    #[test]
    fn classifies_yaml_json_as_suspicious() {
        assert_eq!(classify_file("config.yaml"), FileClass::Suspicious);
        assert_eq!(classify_file("config.yml"), FileClass::Suspicious);
        assert_eq!(classify_file("settings.json"), FileClass::Suspicious);
        assert_eq!(classify_file("k8s/manifest.yaml"), FileClass::Suspicious);
    }

    #[test]
    fn classifies_other_toml_as_suspicious() {
        // Non-devboy, non-Cargo TOML — could be anything.
        assert_eq!(classify_file("ansible/vars.toml"), FileClass::Suspicious);
    }

    #[test]
    fn classifies_shell_scripts_as_suspicious() {
        assert_eq!(classify_file("scripts/run.sh"), FileClass::Suspicious);
        assert_eq!(classify_file("ci/deploy.bash"), FileClass::Suspicious);
    }

    #[test]
    fn unknown_extensions_are_skipped() {
        assert_eq!(classify_file("src/main.rs"), FileClass::Unknown);
        assert_eq!(classify_file("Makefile"), FileClass::Unknown);
        assert_eq!(classify_file("LICENSE"), FileClass::Unknown);
    }

    // -- extract_alias_path --------------------------------------

    #[test]
    fn extract_alias_path_from_quoted_value() {
        assert_eq!(
            extract_alias_path(r#"  token: "@secret:team/gitlab/token-deploy""#).as_deref(),
            Some("team/gitlab/token-deploy")
        );
    }

    #[test]
    fn extract_alias_path_stops_at_quote() {
        // Trailing `"` is not part of the path.
        let p = extract_alias_path(r#"token: "@secret:team/foo/bar""#)
            .expect("alias should be extracted");
        assert_eq!(p, "team/foo/bar");
        assert!(!p.contains('"'));
    }

    #[test]
    fn extract_alias_path_returns_none_when_alias_absent() {
        assert!(extract_alias_path("token: real-static-value").is_none());
    }

    #[test]
    fn extract_alias_path_returns_none_when_no_path_after_prefix() {
        // Bare prefix isn't an alias.
        assert!(extract_alias_path(r#"token: "@secret:""#).is_none());
    }

    // -- find_aliases_in_added_lines ----------------------------

    #[test]
    fn find_aliases_in_added_lines_picks_up_added_alias_with_correct_line_number() {
        // Synthetic unified-diff patch with one hunk starting at
        // line 5 in the new file.
        let patch = "\
diff --git a/config.yaml b/config.yaml
--- a/config.yaml
+++ b/config.yaml
@@ -3,2 +3,3 @@
 unchanged: ok
+token: \"@secret:team/gitlab/token-deploy\"
 unchanged: still ok
";
        let findings = find_aliases_in_added_lines("config.yaml", patch);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].file, "config.yaml");
        assert_eq!(findings[0].new_line, 4);
        assert_eq!(findings[0].alias_path, "team/gitlab/token-deploy");
    }

    #[test]
    fn find_aliases_in_added_lines_ignores_removed_lines() {
        let patch = "\
diff --git a/config.yaml b/config.yaml
--- a/config.yaml
+++ b/config.yaml
@@ -1,2 +1,2 @@
-token: \"@secret:gone/from/here\"
+token: \"replaced-with-static\"
 next: line
";
        let findings = find_aliases_in_added_lines("config.yaml", patch);
        // The `-` line had an alias but is being removed, not
        // committed. The lint must not flag it.
        assert!(findings.is_empty());
    }

    #[test]
    fn find_aliases_in_added_lines_handles_multiple_hits_with_growing_line_numbers() {
        let patch = "\
@@ -1,1 +1,3 @@
+a: \"@secret:foo/bar/baz\"
+b: \"@secret:another/path/here\"
+c: literal
";
        let findings = find_aliases_in_added_lines("x.yaml", patch);
        assert_eq!(findings.len(), 2);
        assert_eq!(findings[0].new_line, 1);
        assert_eq!(findings[0].alias_path, "foo/bar/baz");
        assert_eq!(findings[1].new_line, 2);
        assert_eq!(findings[1].alias_path, "another/path/here");
    }

    #[test]
    fn find_aliases_in_added_lines_skips_diff_metadata_lines() {
        // `+++ b/file` and `--- a/file` must not register as
        // added content.
        let patch = "\
--- a/foo
+++ b/foo
@@ -1 +1 @@
-old
+new
";
        let findings = find_aliases_in_added_lines("foo", patch);
        assert!(findings.is_empty());
    }

    #[test]
    fn parse_new_hunk_start_extracts_starting_line() {
        assert_eq!(parse_new_hunk_start(" -1,2 +5,3 @@"), Some(5));
        assert_eq!(parse_new_hunk_start(" -1,2 +12 @@"), Some(12));
        assert_eq!(parse_new_hunk_start(" garbage"), None);
    }

    // -- install_pre_commit_hook (filesystem only) ---------------

    #[cfg(unix)]
    #[test]
    fn write_hook_file_creates_executable_script() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("hooks").join("pre-commit");
        write_hook_file(&path, PRE_COMMIT_SCRIPT, false).unwrap();
        let perms = std::fs::metadata(&path).unwrap().permissions();
        assert_eq!(perms.mode() & 0o777, 0o755);
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.contains("devboy hooks check secret-alias"));
    }

    #[test]
    fn write_hook_file_refuses_to_clobber_without_force() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("pre-commit");
        std::fs::write(&path, "existing content").unwrap();
        let err = write_hook_file(&path, PRE_COMMIT_SCRIPT, false).unwrap_err();
        assert!(format!("{err:#}").contains("already exists"));
    }

    #[test]
    fn write_hook_file_force_overwrites() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("pre-commit");
        std::fs::write(&path, "existing content").unwrap();
        write_hook_file(&path, PRE_COMMIT_SCRIPT, true).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), PRE_COMMIT_SCRIPT);
    }

    // -- End-to-end with a temp git repo -------------------------

    #[cfg(unix)]
    #[test]
    fn check_secret_alias_flags_alias_in_yaml_in_real_git_repo() {
        if skip_git_dependent_test() {
            return;
        }
        let dir = tempfile::TempDir::new().unwrap();
        let repo = dir.path();

        run_git(repo, &["init", "--quiet"]);
        run_git(repo, &["config", "user.email", "test@example.com"]);
        run_git(repo, &["config", "user.name", "Test"]);
        // First commit so HEAD exists.
        std::fs::write(repo.join("README.md"), "stub\n").unwrap();
        run_git(repo, &["add", "README.md"]);
        run_git(repo, &["commit", "--quiet", "-m", "init"]);

        // Stage a YAML file with an alias.
        std::fs::write(
            repo.join("k8s.yaml"),
            "apiVersion: v1\ntoken: \"@secret:team/x/y\"\n",
        )
        .unwrap();
        run_git(repo, &["add", "k8s.yaml"]);

        let staged = staged_files(Some(repo)).unwrap();
        assert!(staged.iter().any(|(_, p)| p == "k8s.yaml"));

        // Run the check; expect non-zero exit (returned as Err
        // from `run_secret_alias_check` via anyhow::bail).
        let err = run_secret_alias_check(Some(repo)).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("aborting commit"), "unexpected error: {msg}");
    }

    #[cfg(unix)]
    #[test]
    fn check_secret_alias_passes_when_alias_lives_in_devboy_config() {
        if skip_git_dependent_test() {
            return;
        }
        let dir = tempfile::TempDir::new().unwrap();
        let repo = dir.path();

        run_git(repo, &["init", "--quiet"]);
        run_git(repo, &["config", "user.email", "test@example.com"]);
        run_git(repo, &["config", "user.name", "Test"]);
        std::fs::write(repo.join("README.md"), "stub\n").unwrap();
        run_git(repo, &["add", "README.md"]);
        run_git(repo, &["commit", "--quiet", "-m", "init"]);

        // .devboy/secrets.toml is alias-aware — staging an alias
        // there should NOT trip the lint.
        std::fs::create_dir_all(repo.join(".devboy")).unwrap();
        std::fs::write(
            repo.join(".devboy/secrets.toml"),
            "[secret.\"team/x/y\"]\nsource = \"keychain\"\nreference = \"@secret:team/x/y\"\n",
        )
        .unwrap();
        run_git(repo, &["add", ".devboy/secrets.toml"]);

        run_secret_alias_check(Some(repo)).expect("alias-aware file must not trip the lint");
    }

    #[cfg(unix)]
    fn run_git(repo: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(repo)
            .status()
            .expect("failed to invoke git");
        assert!(status.success(), "git {:?} failed", args);
    }

    /// Decide whether to skip a `git`-dependent test. Reasons:
    ///
    /// - `git` not on PATH (some arm runners ship without it).
    /// - Running under `cargo-llvm-cov` (sets `LLVM_PROFILE_FILE`).
    /// - `git --version` succeeds but `git init` in a
    ///   throwaway tempdir doesn't — happens on arm runners
    ///   that ship a git shim with limited subcommand
    ///   coverage.
    ///
    /// The test stays exercised on macOS / ubuntu-latest where
    /// every probe passes.
    #[cfg(unix)]
    fn skip_git_dependent_test() -> bool {
        if std::env::var_os("LLVM_PROFILE_FILE").is_some() {
            eprintln!("skipping: cargo-llvm-cov instrumentation breaks `git` spawn");
            return true;
        }
        let probe_ok = Command::new("git")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !probe_ok {
            eprintln!("skipping: `git` not on PATH (likely an arm-runner sandbox)");
            return true;
        }
        // Real-spawn probe — try `git init` in a fresh tempdir.
        // If that returns `NotFound` even after `--version`
        // worked, we're on a host with a partial git shim.
        let probe_dir = match tempfile::TempDir::new() {
            Ok(d) => d,
            Err(_) => return false,
        };
        match Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(probe_dir.path())
            .status()
        {
            Ok(status) if status.success() => false,
            _ => {
                eprintln!("skipping: `git init` returned NotFound / non-zero in probe dir");
                true
            }
        }
    }
}
