//! Procedural skills on top of the `devboy-tools` bundle.
//!
//! This crate provides:
//!
//! - A [`SkillSource`] trait for pluggable skill backends
//! - An [`EmbeddedSkillSource`] that ships baseline SKILL.md files
//!   inside the binary (via `rust-embed`)
//! - The [`Skill`] / [`Frontmatter`] types, including YAML frontmatter
//!   parsing with a fixed required-field set and extensible unknown
//!   fields
//! - A [`Catalog`] for filtering / counting skill summaries
//! - Per-location install [`Manifest`]s with SHA256-based three-state
//!   collision detection ([`classify`])
//! - The paired [`HistoricalHashes`] registry embedded at build time so
//!   that upgrade flows can distinguish "unchanged", "historical-safe",
//!   and "user-modified" files offline
//!
//! The design decisions are captured, at the repository root, in
//! `docs/architecture/adr/ADR-012-skills-subsystem.md` (architecture +
//! CLI surface), `ADR-013-skills-install-targets.md` (install target
//! resolution), and `ADR-014-skills-lifecycle.md` (manifest,
//! three-state upgrade).

#![deny(missing_docs)]

pub mod catalog;
pub mod embedded;
pub mod error;
pub mod install;
pub mod manifest;
pub mod plugin_dedup;
pub mod skill;
pub mod source;
pub mod trace;

pub use catalog::{Catalog, canonical_skill_name};
pub use embedded::EmbeddedSkillSource;
pub use error::{Result, SkillError};
pub use install::{
    Agent, Environment, InstallOptions, InstallOutcome, InstallReport, InstallSpec, InstallTarget,
    detect_installed_agents, install_skills_to_target, remove_skills_from_target, resolve_targets,
};
pub use manifest::{
    HistoricalHashes, HistoricalVersion, InstallState, InstalledFile, InstalledSkill,
    MANIFEST_FILE, MANIFEST_VERSION, Manifest, SkillHistory, classify, classify_path, sha256_hex,
};
pub use plugin_dedup::{is_claude_plugin_installed, is_codex_plugin_installed};
pub use skill::{Category, Frontmatter, Skill, SkillSummary};
pub use source::SkillSource;
pub use trace::{
    Outcome as TraceOutcome, Phase as TracePhase, SessionMeta, SessionTracer, TraceRecord,
    TraceTarget, append_event, create_session, finalise_session,
};
