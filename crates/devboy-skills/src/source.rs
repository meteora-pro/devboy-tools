//! [`SkillSource`] trait — pluggable backends for skill discovery.
//!
//! The baseline implementation (see [`crate::embedded`]) ships SKILL.md
//! files compiled into the binary. The trait is kept minimal so that
//! external sources (a marketplace, a Langfuse-backed registry) can be
//! added later without reshaping the CLI or the install lifecycle.

use async_trait::async_trait;

use crate::error::Result;
use crate::skill::{Skill, SkillSummary};

/// A pluggable backend that can enumerate and load skills.
///
/// Implementations must be cheap to `list` — the CLI calls `list` on
/// every invocation of `devboy skills list`. `load` is allowed to be
/// heavier (opening files, issuing network calls) because it is only
/// called on explicit install / show.
#[async_trait]
pub trait SkillSource: Send + Sync {
    /// Short identifier for this source ("embedded", "marketplace", …).
    /// Used in error messages and in `devboy skills list --source …`.
    fn name(&self) -> &'static str;

    /// List all skills known to this source. Order is source-defined;
    /// callers should sort if they need a deterministic display.
    async fn list(&self) -> Result<Vec<SkillSummary>>;

    /// Load the full `Skill` (frontmatter + body) for the named skill.
    ///
    /// Returns [`crate::error::SkillError::NotFound`] if the source does
    /// not know about `name`.
    async fn load(&self, name: &str) -> Result<Skill>;
}
