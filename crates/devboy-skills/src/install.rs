//! Install target resolution plus the three-state install / upgrade flow.
//!
//! The resolver turns user-supplied flags (`--global`, `--agent`,
//! `--local`) into a concrete list of on-disk directories where skills
//! should be written. The flow consumes that list together with the
//! embedded [`HistoricalHashes`] registry to apply the three-state
//! install logic from
//! ADR-014 in `docs/architecture/adr/ADR-014-skills-lifecycle.md` at
//! the repository root.

use std::fs;
use std::path::{Path, PathBuf};

use chrono::Utc;

use crate::error::{Result, SkillError};
use crate::manifest::{
    HistoricalHashes, InstallState, InstalledFile, InstalledSkill, MANIFEST_FILE, Manifest,
    classify_path, sha256_hex,
};
use crate::skill::Skill;

// ---------------------------------------------------------------------------
// Supported agents
// ---------------------------------------------------------------------------

/// One of the known agents that has a canonical skills directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Agent {
    /// Claude Code — `~/.claude/skills/` (user) or `./.claude/skills/`
    /// (project).
    Claude,
    /// Codex — `~/.codex/skills/` (user) or `./.codex/skills/`
    /// (project).
    Codex,
    /// Cursor — `~/.cursor/skills/` (user) or `./.cursor/skills/`
    /// (project).
    Cursor,
    /// Kimi — `~/.kimi/skills/` (user) or `./.kimi/skills/` (project).
    Kimi,
}

impl Agent {
    /// Canonical spelling of the agent id (same as the CLI flag value).
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Cursor => "cursor",
            Self::Kimi => "kimi",
        }
    }

    /// Every agent this crate knows about.
    pub fn all() -> &'static [Agent] {
        &[Self::Claude, Self::Codex, Self::Cursor, Self::Kimi]
    }

    /// Parse a flag value (`claude`, `codex`, `cursor`, `kimi`).
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "claude" => Some(Self::Claude),
            "codex" => Some(Self::Codex),
            "cursor" => Some(Self::Cursor),
            "kimi" => Some(Self::Kimi),
            _ => None,
        }
    }

    /// Name of the hidden directory the agent reads from (e.g.
    /// `.claude`, `.codex`, …).
    pub fn dir_name(&self) -> &'static str {
        match self {
            Self::Claude => ".claude",
            Self::Codex => ".codex",
            Self::Cursor => ".cursor",
            Self::Kimi => ".kimi",
        }
    }
}

/// Detect which agents have a home directory on this machine. Used by
/// `--agent all` to decide where to install.
pub fn detect_installed_agents(home: &Path) -> Vec<Agent> {
    Agent::all()
        .iter()
        .copied()
        .filter(|a| home.join(a.dir_name()).is_dir())
        .collect()
}

// ---------------------------------------------------------------------------
// Install target specification
// ---------------------------------------------------------------------------

/// Specification of where to install (from CLI flags), before resolution.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InstallSpec {
    /// Whether `--global` was passed.
    pub global: bool,
    /// Whether `--local` was passed.
    pub local: bool,
    /// Agents named by `--agent` (possibly multiple; `--agent all`
    /// expands to every detected agent).
    pub agents: Vec<Agent>,
    /// When `--agent all` was passed, we also install to the
    /// vendor-neutral `~/.agents/skills/` location — this flag records
    /// that intent so the resolver can produce that extra target.
    pub include_vendor_neutral_global: bool,
}

/// A concrete install target — a single directory on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallTarget {
    /// Human-readable label used in logs / CLI output.
    pub label: String,
    /// Parent directory that will hold the per-skill subdirectories.
    ///
    /// Example: `/home/alice/.agents/skills` or
    /// `/path/to/repo/.agents/skills`.
    pub skills_dir: PathBuf,
}

/// Paths the resolver needs from the environment — factored out so
/// tests can inject their own without touching the real filesystem.
#[derive(Debug, Clone)]
pub struct Environment {
    pub cwd: PathBuf,
    /// User's home directory (typically `dirs::home_dir()`).
    pub home: PathBuf,
    /// Root of the current repository, if any.
    pub repo_root: Option<PathBuf>,
}

impl Environment {
    /// Build an [`Environment`] from the real operating system.
    ///
    /// The `DEVBOY_HOME_OVERRIDE` environment variable, when set,
    /// replaces the detected home directory. This exists mostly for
    /// integration tests that want to redirect installs into a
    /// temporary directory on platforms (notably Windows) where
    /// `dirs::home_dir()` resolves through the OS rather than through
    /// `$HOME` / `$USERPROFILE`.
    pub fn detect() -> Result<Self> {
        let cwd = std::env::current_dir().map_err(|source| SkillError::Io {
            path: PathBuf::from("."),
            source,
        })?;
        let home = match std::env::var_os("DEVBOY_HOME_OVERRIDE") {
            Some(p) if !p.is_empty() => PathBuf::from(p),
            _ => dirs::home_dir().ok_or_else(|| SkillError::Io {
                path: PathBuf::from("~"),
                source: std::io::Error::other("home directory is not set"),
            })?,
        };
        let repo_root = locate_repo_root(&cwd);
        Ok(Self {
            cwd,
            home,
            repo_root,
        })
    }
}

fn locate_repo_root(start: &Path) -> Option<PathBuf> {
    let mut cur = start;
    loop {
        if cur.join(".git").exists() || cur.join(".devboy.toml").exists() {
            return Some(cur.to_path_buf());
        }
        cur = cur.parent()?;
    }
}

// ---------------------------------------------------------------------------
// Resolver
// ---------------------------------------------------------------------------

/// Resolve an [`InstallSpec`] into one or more concrete [`InstallTarget`]s.
///
/// Rules (ADR-013):
///
/// - Default (no flags) — repo-local at `<repo>/.agents/skills/`.
/// - `--global` — `~/.agents/skills/`.
/// - `--agent X` — agent's home path unless `--local` was also passed,
///   in which case `<repo>/<agent-dir>/skills/`.
/// - `--agent all` — every detected agent plus `~/.agents/skills/`.
///
/// When neither `--global` nor `--agent` is passed and the environment
/// has no repository root, this function returns
/// [`SkillError::MissingRequiredField`] (reusing that variant as the
/// "user must pick a target" signal; the CLI layer formats it into the
/// helpful multi-line error described in the ADR).
pub fn resolve_targets(env: &Environment, spec: &InstallSpec) -> Result<Vec<InstallTarget>> {
    let mut targets: Vec<InstallTarget> = Vec::new();

    if spec.global {
        targets.push(InstallTarget {
            label: "global (~/.agents/skills)".into(),
            skills_dir: env.home.join(".agents").join("skills"),
        });
    }

    if !spec.agents.is_empty() {
        for agent in &spec.agents {
            let (label, root) = if spec.local {
                let repo =
                    env.repo_root
                        .as_ref()
                        .ok_or_else(|| SkillError::MissingRequiredField {
                            skill: "<install-target>".into(),
                            field: "repository root",
                        })?;
                (
                    format!(
                        "{} (local: <repo>/{}/skills)",
                        agent.as_str(),
                        agent.dir_name()
                    ),
                    repo.join(agent.dir_name()).join("skills"),
                )
            } else {
                (
                    format!("{} (~/{}/skills)", agent.as_str(), agent.dir_name()),
                    env.home.join(agent.dir_name()).join("skills"),
                )
            };
            targets.push(InstallTarget {
                label,
                skills_dir: root,
            });
        }
        if spec.include_vendor_neutral_global
            && !targets
                .iter()
                .any(|t| t.skills_dir == env.home.join(".agents").join("skills"))
        {
            targets.push(InstallTarget {
                label: "global (~/.agents/skills)".into(),
                skills_dir: env.home.join(".agents").join("skills"),
            });
        }
    }

    if !spec.global && spec.agents.is_empty() {
        let repo = env
            .repo_root
            .as_ref()
            .ok_or_else(|| SkillError::MissingRequiredField {
                skill: "<install-target>".into(),
                field: "repository root (pass --global or --agent to install outside a repo)",
            })?;
        targets.push(InstallTarget {
            label: "repo-local (<repo>/.agents/skills)".into(),
            skills_dir: repo.join(".agents").join("skills"),
        });
    }

    Ok(targets)
}

// ---------------------------------------------------------------------------
// Install / upgrade flow
// ---------------------------------------------------------------------------

/// Options for a single install pass at a single target.
#[derive(Debug, Clone, Default)]
pub struct InstallOptions {
    /// Overwrite files classified as `UserModified` / `Unknown`.
    pub force: bool,
    /// Do not write anything; just compute the plan.
    pub dry_run: bool,
    /// Label to record in the manifest as the originating devboy
    /// version.
    pub installed_from: Option<String>,
}

/// Per-skill result of a single install attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallOutcome {
    /// Skill was newly written to disk.
    Installed,
    /// File already matches the current shipped version.
    Unchanged,
    /// Existing file was one of our previous shipped versions — auto-upgraded.
    Upgraded {
        /// Version recorded in the manifest before the upgrade (if any).
        from_version: Option<u32>,
    },
    /// Existing file was user-modified; overwrite refused.
    SkippedUserModified,
    /// `--force` was set; user-modified file was overwritten anyway.
    OverwrittenWithForce,
    /// Skill is unknown to the embedded history AND already has a
    /// file on disk with an unknown hash — nothing classified, nothing
    /// changed.
    SkippedUnknown,
}

/// Aggregate result across every skill installed to a target.
#[derive(Debug, Clone, Default)]
pub struct InstallReport {
    /// Per-skill outcomes keyed by skill name.
    pub outcomes: std::collections::BTreeMap<String, InstallOutcome>,
}

impl InstallReport {
    /// Count of skills with a given outcome.
    pub fn count(&self, outcome: &InstallOutcome) -> usize {
        self.outcomes.values().filter(|o| o == &outcome).count()
    }

    /// `true` if nothing was installed or upgraded.
    pub fn is_all_noop(&self) -> bool {
        self.outcomes.values().all(|o| {
            matches!(
                o,
                InstallOutcome::Unchanged
                    | InstallOutcome::SkippedUserModified
                    | InstallOutcome::SkippedUnknown
            )
        })
    }
}

/// Install a set of skills to a single target directory.
pub fn install_skills_to_target(
    target: &InstallTarget,
    skills: &[Skill],
    history: &HistoricalHashes,
    options: &InstallOptions,
) -> Result<InstallReport> {
    if !options.dry_run {
        fs::create_dir_all(&target.skills_dir).map_err(|source| SkillError::Io {
            path: target.skills_dir.clone(),
            source,
        })?;
    }

    let manifest_path = target.skills_dir.join(MANIFEST_FILE);
    // `Manifest::load` returns an empty manifest only when the file does
    // not exist. A corrupt manifest is propagated so the caller sees the
    // parse error rather than silently discarding every install record.
    let mut manifest = Manifest::load(&manifest_path)?;
    manifest.installed_from = options.installed_from.clone().or(manifest.installed_from);

    let mut report = InstallReport::default();

    for skill in skills {
        let skill_dir = target.skills_dir.join(&skill.frontmatter.name);
        let skill_path = skill_dir.join("SKILL.md");
        let body = render_skill_file(skill);

        // First, try to classify against the embedded history.
        let mut state = classify_path(history, &skill.frontmatter.name, &skill_path)?
            .unwrap_or(InstallState::Unknown);

        // Fallback: when the embedded history is empty for this skill,
        // the manifest is still a trustworthy source of truth about
        // "did we install this, and has it changed since?". Promote
        // matching-manifest-hash to Unchanged; keep everything else as
        // classified.
        if matches!(state, InstallState::Unknown)
            && let Some(recorded) = manifest.get(&skill.frontmatter.name)
            && let Some(file_entry) = recorded.files.get("SKILL.md")
            && let Some(actual_sha) = hash_file_if_exists(&skill_path)?
        {
            state = if actual_sha.eq_ignore_ascii_case(&file_entry.sha256) {
                InstallState::Unchanged
            } else {
                InstallState::UserModified
            };
        }

        let previously_installed = skill_path.is_file();

        let outcome = match (state, previously_installed, options.force) {
            (_, false, _) => {
                write_skill(&skill_dir, &skill_path, body.as_bytes(), options.dry_run)?;
                if !options.dry_run {
                    manifest.record(
                        &skill.frontmatter.name,
                        record_for(skill, body.as_bytes(), options, "embedded"),
                    );
                }
                InstallOutcome::Installed
            }
            (InstallState::Unchanged, true, _) => InstallOutcome::Unchanged,
            (InstallState::HistoricalSafe, true, _) => {
                let prev_version = manifest.get(&skill.frontmatter.name).map(|m| m.version);
                write_skill(&skill_dir, &skill_path, body.as_bytes(), options.dry_run)?;
                if !options.dry_run {
                    manifest.record(
                        &skill.frontmatter.name,
                        record_for(skill, body.as_bytes(), options, "embedded"),
                    );
                }
                InstallOutcome::Upgraded {
                    from_version: prev_version,
                }
            }
            (InstallState::UserModified, true, true) => {
                write_skill(&skill_dir, &skill_path, body.as_bytes(), options.dry_run)?;
                if !options.dry_run {
                    manifest.record(
                        &skill.frontmatter.name,
                        record_for(skill, body.as_bytes(), options, "embedded"),
                    );
                }
                InstallOutcome::OverwrittenWithForce
            }
            (InstallState::UserModified, true, false) => InstallOutcome::SkippedUserModified,
            (InstallState::Unknown, true, true) => {
                write_skill(&skill_dir, &skill_path, body.as_bytes(), options.dry_run)?;
                if !options.dry_run {
                    manifest.record(
                        &skill.frontmatter.name,
                        record_for(skill, body.as_bytes(), options, "embedded"),
                    );
                }
                InstallOutcome::OverwrittenWithForce
            }
            (InstallState::Unknown, true, false) => InstallOutcome::SkippedUnknown,
        };

        report
            .outcomes
            .insert(skill.frontmatter.name.clone(), outcome);
    }

    if !options.dry_run {
        manifest.save(&manifest_path)?;
    }

    Ok(report)
}

/// Remove installed skills from a target, clearing manifest entries.
/// Missing skills are silently ignored unless `strict` is set.
pub fn remove_skills_from_target(
    target: &InstallTarget,
    names: &[String],
    strict: bool,
    dry_run: bool,
) -> Result<Vec<String>> {
    let manifest_path = target.skills_dir.join(MANIFEST_FILE);
    // Manifest parse errors propagate; a missing file produces an empty
    // manifest as for `install`.
    let mut manifest = Manifest::load(&manifest_path)?;
    let mut removed = Vec::new();

    for name in names {
        let skill_dir = target.skills_dir.join(name);
        let dir_present = skill_dir.exists();
        let in_manifest = manifest.get(name).is_some();

        if !dir_present && !in_manifest {
            if strict {
                return Err(SkillError::NotFound {
                    name: name.clone(),
                    source_name: "install-target",
                });
            }
            continue;
        }

        if dir_present && !dry_run {
            fs::remove_dir_all(&skill_dir).map_err(|source| SkillError::Io {
                path: skill_dir.clone(),
                source,
            })?;
        }
        // Always clean the manifest entry — if the directory vanished
        // out of band, the manifest row becomes stale and callers see
        // the skill as installed forever. This brings the two sources
        // of truth back in sync.
        if !dry_run {
            manifest.forget(name);
        }
        removed.push(name.clone());
    }

    if !dry_run {
        manifest.save(&manifest_path)?;
    }
    Ok(removed)
}

// ---------------------------------------------------------------------------
// Legacy name migration
// ---------------------------------------------------------------------------

/// Result of `scan_legacy_skills_at_target` for one entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacySkill {
    /// Legacy directory name on disk, e.g. `devboy-setup`.
    pub legacy_name: String,
    /// New name without the `devboy-` prefix, e.g. `setup`.
    pub canonical_name: String,
    /// `true` if a sibling directory with the canonical name already
    /// exists at the same target (meaning the legacy entry is a safe
    /// duplicate to remove).
    pub canonical_present: bool,
    /// Absolute path of the legacy skill directory.
    pub path: PathBuf,
}

/// Find `devboy-<name>` directories at a target where the new
/// canonical entry `<name>` is also present (a safe-to-remove
/// duplicate). Directories without a sibling are returned with
/// `canonical_present: false` and the caller decides what to do.
pub fn scan_legacy_skills_at_target(target: &InstallTarget) -> Result<Vec<LegacySkill>> {
    let mut out = Vec::new();
    let entries = match fs::read_dir(&target.skills_dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(source) => {
            return Err(SkillError::Io {
                path: target.skills_dir.clone(),
                source,
            });
        }
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(canonical) = name.strip_prefix("devboy-").map(str::to_owned) else {
            continue;
        };
        // Only consider directories (real or symlinks to dirs); skip files.
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let canonical_path = target.skills_dir.join(&canonical);
        out.push(LegacySkill {
            legacy_name: name,
            canonical_name: canonical,
            canonical_present: canonical_path.exists(),
            path,
        });
    }
    out.sort_by(|a, b| a.legacy_name.cmp(&b.legacy_name));
    Ok(out)
}

/// Remove legacy `devboy-<name>` skill directories at a target where a
/// canonical sibling `<name>` is present (safe duplicates). Returns the
/// list of legacy names actually removed. Skips entries without a
/// canonical sibling — those are surfaced by the caller as warnings.
///
/// `dry_run = true` reports what would be removed without touching the
/// filesystem.
pub fn migrate_legacy_skills_at_target(
    target: &InstallTarget,
    dry_run: bool,
) -> Result<Vec<String>> {
    let scan = scan_legacy_skills_at_target(target)?;
    let safe: Vec<String> = scan
        .iter()
        .filter(|s| s.canonical_present)
        .map(|s| s.legacy_name.clone())
        .collect();
    if safe.is_empty() {
        return Ok(safe);
    }
    remove_skills_from_target(target, &safe, false, dry_run)
}

// ---------------------------------------------------------------------------
// Internals
// ---------------------------------------------------------------------------

fn hash_file_if_exists(path: &Path) -> Result<Option<String>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(sha256_hex(&bytes))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(SkillError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn render_skill_file(skill: &Skill) -> String {
    // Round-trip the frontmatter through serde_yaml so we normalise
    // ordering and any `extra` fields round-trip correctly.
    let yaml =
        serde_yaml::to_string(&skill.frontmatter).expect("frontmatter should always serialise");
    let mut out = String::with_capacity(yaml.len() + skill.body.len() + 16);
    out.push_str("---\n");
    out.push_str(&yaml);
    if !yaml.ends_with('\n') {
        out.push('\n');
    }
    out.push_str("---\n");
    out.push_str(&skill.body);
    out
}

fn write_skill(skill_dir: &Path, file_path: &Path, bytes: &[u8], dry_run: bool) -> Result<()> {
    if dry_run {
        return Ok(());
    }
    fs::create_dir_all(skill_dir).map_err(|source| SkillError::Io {
        path: skill_dir.to_path_buf(),
        source,
    })?;
    fs::write(file_path, bytes).map_err(|source| SkillError::Io {
        path: file_path.to_path_buf(),
        source,
    })?;
    Ok(())
}

fn record_for(
    skill: &Skill,
    body: &[u8],
    _options: &InstallOptions,
    source: &str,
) -> InstalledSkill {
    let mut files = std::collections::BTreeMap::new();
    files.insert(
        "SKILL.md".to_string(),
        InstalledFile {
            sha256: sha256_hex(body),
            size: body.len() as u64,
        },
    );
    // `InstalledSkill.source` records which `SkillSource` produced the
    // skill (`"embedded"`, a future `"marketplace"`, `"langfuse"` etc.).
    // The devboy-tools version that did the install lives in the
    // top-level `Manifest.installed_from` field — keeping the two
    // distinct is deliberate.
    InstalledSkill {
        version: skill.frontmatter.version,
        installed_at: Utc::now(),
        source: source.to_string(),
        files,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn test_env(home: &Path, cwd: &Path, repo_root: Option<PathBuf>) -> Environment {
        Environment {
            cwd: cwd.to_path_buf(),
            home: home.to_path_buf(),
            repo_root,
        }
    }

    #[test]
    fn resolver_default_is_repo_local() {
        let home = tempdir().unwrap();
        let repo = tempdir().unwrap();
        let env = test_env(home.path(), repo.path(), Some(repo.path().to_path_buf()));
        let spec = InstallSpec::default();
        let targets = resolve_targets(&env, &spec).unwrap();
        assert_eq!(targets.len(), 1);
        assert_eq!(
            targets[0].skills_dir,
            repo.path().join(".agents").join("skills")
        );
    }

    #[test]
    fn resolver_fails_without_repo_and_flags() {
        let home = tempdir().unwrap();
        let env = test_env(home.path(), home.path(), None);
        let spec = InstallSpec::default();
        let err = resolve_targets(&env, &spec).unwrap_err();
        assert!(
            matches!(err, SkillError::MissingRequiredField { .. }),
            "expected MissingRequiredField, got {err:?}"
        );
    }

    #[test]
    fn resolver_global() {
        let home = tempdir().unwrap();
        let env = test_env(home.path(), home.path(), None);
        let spec = InstallSpec {
            global: true,
            ..Default::default()
        };
        let targets = resolve_targets(&env, &spec).unwrap();
        assert_eq!(targets.len(), 1);
        assert_eq!(
            targets[0].skills_dir,
            home.path().join(".agents").join("skills")
        );
    }

    #[test]
    fn resolver_agent_maps_to_home() {
        let home = tempdir().unwrap();
        let env = test_env(home.path(), home.path(), None);
        let spec = InstallSpec {
            agents: vec![Agent::Claude],
            ..Default::default()
        };
        let targets = resolve_targets(&env, &spec).unwrap();
        assert_eq!(targets.len(), 1);
        assert_eq!(
            targets[0].skills_dir,
            home.path().join(".claude").join("skills")
        );
    }

    #[test]
    fn resolver_agent_local_maps_to_repo() {
        let home = tempdir().unwrap();
        let repo = tempdir().unwrap();
        let env = test_env(home.path(), repo.path(), Some(repo.path().to_path_buf()));
        let spec = InstallSpec {
            agents: vec![Agent::Codex],
            local: true,
            ..Default::default()
        };
        let targets = resolve_targets(&env, &spec).unwrap();
        assert_eq!(targets.len(), 1);
        assert_eq!(
            targets[0].skills_dir,
            repo.path().join(".codex").join("skills")
        );
    }

    #[test]
    fn resolver_agent_all_expands_and_includes_vendor_neutral() {
        let home = tempdir().unwrap();
        let env = test_env(home.path(), home.path(), None);
        let spec = InstallSpec {
            agents: vec![Agent::Claude, Agent::Codex],
            include_vendor_neutral_global: true,
            ..Default::default()
        };
        let targets = resolve_targets(&env, &spec).unwrap();
        assert_eq!(targets.len(), 3);
        let paths: Vec<_> = targets.iter().map(|t| t.skills_dir.clone()).collect();
        assert!(paths.contains(&home.path().join(".claude").join("skills")));
        assert!(paths.contains(&home.path().join(".codex").join("skills")));
        assert!(paths.contains(&home.path().join(".agents").join("skills")));
    }

    #[test]
    fn detect_installed_agents_filters_by_dir_presence() {
        let home = tempdir().unwrap();
        fs::create_dir(home.path().join(".claude")).unwrap();
        fs::create_dir(home.path().join(".cursor")).unwrap();
        let detected = detect_installed_agents(home.path());
        assert!(detected.contains(&Agent::Claude));
        assert!(detected.contains(&Agent::Cursor));
        assert!(!detected.contains(&Agent::Codex));
        assert!(!detected.contains(&Agent::Kimi));
    }

    #[test]
    fn install_writes_new_skill_and_skips_unchanged() {
        let dir = tempdir().unwrap();
        let target = InstallTarget {
            label: "test".into(),
            skills_dir: dir.path().to_path_buf(),
        };
        let skill = crate::skill::Skill::parse(
            "setup",
            r#"---
name: setup
description: test
category: self-bootstrap
version: 1
---
body
"#,
        )
        .unwrap();

        // Prepare history so the rendered-current matches.
        let body = render_skill_file(&skill);
        let mut history = HistoricalHashes::default();
        history.by_skill.insert(
            "setup".into(),
            crate::manifest::SkillHistory {
                current: crate::manifest::HistoricalVersion {
                    version: 1,
                    sha256: sha256_hex(body.as_bytes()),
                },
                history: vec![],
            },
        );

        let options = InstallOptions::default();
        let report =
            install_skills_to_target(&target, std::slice::from_ref(&skill), &history, &options)
                .unwrap();
        assert_eq!(
            report.outcomes.get("setup"),
            Some(&InstallOutcome::Installed)
        );

        // Second run: file on disk now matches current; outcome = Unchanged.
        let report = install_skills_to_target(&target, &[skill], &history, &options).unwrap();
        assert_eq!(
            report.outcomes.get("setup"),
            Some(&InstallOutcome::Unchanged)
        );
    }

    #[test]
    fn install_refuses_user_modification_without_force() {
        let dir = tempdir().unwrap();
        let target = InstallTarget {
            label: "test".into(),
            skills_dir: dir.path().to_path_buf(),
        };
        let skill = crate::skill::Skill::parse(
            "setup",
            r#"---
name: setup
description: test
category: self-bootstrap
version: 1
---
body
"#,
        )
        .unwrap();

        // Plant a user-modified file.
        let skill_dir = dir.path().join("setup");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(skill_dir.join("SKILL.md"), b"user version").unwrap();

        // History knows setup but neither the current nor any
        // historical hash matches "user version".
        let mut history = HistoricalHashes::default();
        history.by_skill.insert(
            "setup".into(),
            crate::manifest::SkillHistory {
                current: crate::manifest::HistoricalVersion {
                    version: 1,
                    sha256: sha256_hex(b"current shipped body"),
                },
                history: vec![],
            },
        );

        let report = install_skills_to_target(
            &target,
            std::slice::from_ref(&skill),
            &history,
            &InstallOptions::default(),
        )
        .unwrap();
        assert_eq!(
            report.outcomes.get("setup"),
            Some(&InstallOutcome::SkippedUserModified)
        );

        // With --force the same call overwrites.
        let report = install_skills_to_target(
            &target,
            &[skill],
            &history,
            &InstallOptions {
                force: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            report.outcomes.get("setup"),
            Some(&InstallOutcome::OverwrittenWithForce)
        );
    }

    #[test]
    fn remove_deletes_skill_and_manifest_entry() {
        let dir = tempdir().unwrap();
        let target = InstallTarget {
            label: "test".into(),
            skills_dir: dir.path().to_path_buf(),
        };
        let skill_dir = dir.path().join("setup");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(skill_dir.join("SKILL.md"), b"hi").unwrap();

        let removed = remove_skills_from_target(&target, &["setup".into()], false, false).unwrap();
        assert_eq!(removed, vec!["setup".to_string()]);
        assert!(!skill_dir.exists());
    }

    // -------------------------------------------------------------------
    // Agent parsing / detection
    // -------------------------------------------------------------------

    #[test]
    fn agent_parse_and_as_str_round_trip() {
        for agent in Agent::all() {
            let parsed = Agent::parse(agent.as_str()).unwrap();
            assert_eq!(parsed, *agent);
            assert!(!agent.dir_name().is_empty());
            assert!(agent.dir_name().starts_with('.'));
        }
        assert!(Agent::parse("not-an-agent").is_none());
        // `all` advertises exactly the four agents enumerated today —
        // bumping this count is intentional and should be done
        // alongside a matching `parse` arm.
        assert_eq!(Agent::all().len(), 4);
    }

    #[test]
    fn detect_installed_agents_filters_on_disk() {
        let home = tempdir().unwrap();
        // No agent dirs created → nothing detected.
        assert!(detect_installed_agents(home.path()).is_empty());

        // Drop two agent directories; expect exactly those back.
        fs::create_dir_all(home.path().join(".claude")).unwrap();
        fs::create_dir_all(home.path().join(".kimi")).unwrap();
        let detected = detect_installed_agents(home.path());
        assert!(detected.contains(&Agent::Claude));
        assert!(detected.contains(&Agent::Kimi));
        assert!(!detected.contains(&Agent::Codex));
    }

    // -------------------------------------------------------------------
    // render_skill_file
    // -------------------------------------------------------------------

    #[test]
    fn render_skill_file_produces_round_trippable_markdown() {
        let original = r#"---
name: setup
description: Walk the user through initial devboy configuration.
category: self-bootstrap
version: 3
compatibility: devboy-tools >= 0.18
activation:
  - "setup devboy"
tools:
  - doctor
---

# setup

Body stays intact across a render round-trip.
"#;
        let skill = Skill::parse("setup", original).unwrap();
        let rendered = render_skill_file(&skill);
        assert!(rendered.starts_with("---\n"));
        assert!(rendered.contains("\n---\n"));
        assert!(rendered.contains("Body stays intact"));

        // Re-parse: the rendered file must round-trip back to the same
        // frontmatter + body pair.
        let reparsed = Skill::parse("setup", &rendered).unwrap();
        assert_eq!(reparsed.name(), skill.name());
        assert_eq!(reparsed.version(), skill.version());
        assert_eq!(reparsed.category(), skill.category());
        assert_eq!(reparsed.body.trim_end(), skill.body.trim_end());
    }

    // -------------------------------------------------------------------
    // Legacy migration
    // -------------------------------------------------------------------

    fn plant_skill(target: &InstallTarget, name: &str) {
        let dir = target.skills_dir.join(name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("SKILL.md"),
            format!(
                "---\nname: {n}\ndescription: x\ncategory: self-bootstrap\nversion: 1\n---\nbody\n",
                n = name
            ),
        )
        .unwrap();
    }

    #[test]
    fn scan_finds_legacy_dirs_and_reports_canonical_presence() {
        let dir = tempdir().unwrap();
        let target = InstallTarget {
            label: "test".into(),
            skills_dir: dir.path().to_path_buf(),
        };
        // Legacy with sibling — safe to remove.
        plant_skill(&target, "devboy-setup");
        plant_skill(&target, "setup");
        // Legacy without sibling — caller decides.
        plant_skill(&target, "devboy-orphan");
        // New-style entry — should be ignored by the scanner.
        plant_skill(&target, "review-mr");
        // A regular file with `devboy-` prefix — must NOT be flagged
        // (the scanner only considers directories).
        fs::write(dir.path().join("devboy-readme.txt"), "hi").unwrap();

        let scan = scan_legacy_skills_at_target(&target).unwrap();
        let names: Vec<&str> = scan.iter().map(|s| s.legacy_name.as_str()).collect();
        assert_eq!(names, vec!["devboy-orphan", "devboy-setup"]);
        let setup = scan
            .iter()
            .find(|s| s.legacy_name == "devboy-setup")
            .unwrap();
        assert!(setup.canonical_present);
        assert_eq!(setup.canonical_name, "setup");
        let orphan = scan
            .iter()
            .find(|s| s.legacy_name == "devboy-orphan")
            .unwrap();
        assert!(!orphan.canonical_present);
    }

    #[test]
    fn scan_returns_empty_when_target_dir_missing() {
        let target = InstallTarget {
            label: "test".into(),
            skills_dir: PathBuf::from("/definitely/does/not/exist"),
        };
        let scan = scan_legacy_skills_at_target(&target).unwrap();
        assert!(scan.is_empty());
    }

    #[test]
    fn migrate_removes_safe_duplicates_only() {
        let dir = tempdir().unwrap();
        let target = InstallTarget {
            label: "test".into(),
            skills_dir: dir.path().to_path_buf(),
        };
        plant_skill(&target, "devboy-setup");
        plant_skill(&target, "setup");
        plant_skill(&target, "devboy-orphan");

        let removed = migrate_legacy_skills_at_target(&target, false).unwrap();
        assert_eq!(removed, vec!["devboy-setup".to_string()]);

        // The safe duplicate is gone; the canonical and the orphan stay.
        assert!(!dir.path().join("devboy-setup").exists());
        assert!(dir.path().join("setup").exists());
        assert!(dir.path().join("devboy-orphan").exists());
    }

    #[test]
    fn migrate_dry_run_does_not_touch_filesystem() {
        let dir = tempdir().unwrap();
        let target = InstallTarget {
            label: "test".into(),
            skills_dir: dir.path().to_path_buf(),
        };
        plant_skill(&target, "devboy-setup");
        plant_skill(&target, "setup");

        let would_remove = migrate_legacy_skills_at_target(&target, true).unwrap();
        assert_eq!(would_remove, vec!["devboy-setup".to_string()]);
        assert!(dir.path().join("devboy-setup").exists());
        assert!(dir.path().join("setup").exists());
    }

    #[test]
    fn migrate_is_noop_when_nothing_legacy() {
        let dir = tempdir().unwrap();
        let target = InstallTarget {
            label: "test".into(),
            skills_dir: dir.path().to_path_buf(),
        };
        plant_skill(&target, "setup");
        plant_skill(&target, "review-mr");
        let removed = migrate_legacy_skills_at_target(&target, false).unwrap();
        assert!(removed.is_empty());
        assert!(dir.path().join("setup").exists());
        assert!(dir.path().join("review-mr").exists());
    }
}
