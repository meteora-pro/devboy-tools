//! Agent detection.
//!
//! Walks `$HOME/` looking for deterministic on-disk traces of supported AI
//! coding agents (Claude Code, GitHub Copilot CLI, Codex CLI, Kimi Code CLI,
//! Cursor, Gemini CLI, Antigravity), and reports a structured snapshot per
//! agent — install status, session count, last-used timestamp, and a score
//! used to pick a primary candidate for `devboy onboard`.
//!
//! Architecture: one [`AgentDetector`] implementation per agent file, plus
//! a [`registry::detect_all`] entrypoint that runs every detector
//! sequentially and returns a sorted [`AgentSnapshot`] vector. Each detector
//! is bound on filesystem I/O (a handful of `read_dir` + `metadata` calls);
//! the total wall-clock is well under a second on a real machine, so we
//! don't add a thread pool today.
//!
//! See ADR-017.

pub mod antigravity;
pub mod bundles;
pub mod claude;
pub mod codex;
pub mod copilot;
pub mod cursor;
pub mod gemini;
pub mod kimi;
pub mod registry;
pub mod score;

mod fs_util;

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::Serialize;

pub use registry::{detect_all, detect_all_with_home};
pub use score::{compute_score, pick_primary};

/// Whether an agent is installed on the machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum InstallStatus {
    /// The agent's install marker (config dir or binary) was found.
    Yes,
    /// No traces found.
    No,
    /// We couldn't determine — e.g. the marker exists but is empty/ambiguous.
    Unknown,
}

/// One row in the detector's report.
#[derive(Debug, Clone, Serialize)]
pub struct AgentSnapshot {
    pub id: &'static str,
    pub display_name: &'static str,
    pub status: InstallStatus,
    pub sessions: Option<u64>,
    pub last_used: Option<DateTime<Utc>>,
    pub score: f64,
    pub paths_checked: Vec<PathBuf>,
}

/// Trait every per-agent detector implements.
pub trait AgentDetector: Send + Sync {
    /// Fn.
    fn id(&self) -> &'static str;
    /// Fn.
    fn display_name(&self) -> &'static str;

    /// Inspect `home` (the user's home directory) and produce a snapshot.
    /// Detectors must not panic on missing paths — `InstallStatus::No` is the
    /// expected outcome for absent agents.
    fn detect(&self, home: &Path) -> AgentSnapshot;
}
