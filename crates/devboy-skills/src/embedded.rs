//! [`EmbeddedSkillSource`] — baseline skills compiled into the binary.
//!
//! The on-disk layout is the `skills/` tree at the repository root. At
//! build time [`rust_embed`] pulls every `SKILL.md` under that tree into
//! the binary so installed users never need the repository itself.
//!
//! The matching install-time historical hash registry lives next to this
//! module (see [`crate::manifest::HistoricalHashes`]) — the two are kept
//! in lock-step so an upgrade can tell whether a file on disk is the
//! current shipped version, a previous shipped version (safe to
//! auto-upgrade), or a user modification.

use std::collections::BTreeMap;

use async_trait::async_trait;
use rust_embed::RustEmbed;

use crate::error::{Result, SkillError};
use crate::skill::{Skill, SkillSummary};
use crate::source::SkillSource;

/// Embedder wrapper around the in-crate `skills/` directory.
///
/// The `RustEmbed` folder path is relative to the crate root. The tree
/// lives inside the crate (at `crates/devboy-skills/skills/`) so that
/// `cargo publish` can package it — see ADR-022 for the rationale.
#[derive(RustEmbed)]
#[folder = "skills/"]
#[include = "*/*/SKILL.md"]
struct BaselineAssets;

/// Source that reads skills embedded into the binary at build time.
#[derive(Default, Clone)]
pub struct EmbeddedSkillSource;

impl EmbeddedSkillSource {
    /// Create a new embedded source. Construction is free — all work
    /// happens on `list` / `load`.
    pub fn new() -> Self {
        Self
    }

    /// Iterate over the (skill-name, file-bytes) pairs produced by the
    /// embedder, skipping anything that does not match the
    /// `<category>/<skill>/SKILL.md` shape.
    fn iter() -> impl Iterator<Item = (String, Vec<u8>)> {
        BaselineAssets::iter().filter_map(|path| {
            let path_str = path.as_ref().to_string();
            // Expect exactly three path segments (category, skill, SKILL.md).
            let parts: Vec<&str> = path_str.split('/').collect();
            if parts.len() != 3 || parts[2] != "SKILL.md" {
                return None;
            }
            let skill_dir = parts[1].to_string();
            BaselineAssets::get(&path_str).map(|f| (skill_dir, f.data.into_owned()))
        })
    }

    fn load_skill(name: &str) -> Result<Skill> {
        let contents = Self::iter()
            .find(|(n, _)| n == name)
            .map(|(_, bytes)| bytes)
            .ok_or_else(|| SkillError::NotFound {
                name: name.to_string(),
                source_name: "embedded",
            })?;
        let text = String::from_utf8(contents).map_err(|e| SkillError::InvalidFieldType {
            skill: name.to_string(),
            field: "<body>",
            reason: format!("SKILL.md is not valid UTF-8: {e}"),
        })?;
        Skill::parse(name, &text)
    }

    /// Return every embedded skill, keyed by skill name. Useful for
    /// callers that want to iterate without going through
    /// [`SkillSource::list`].
    pub fn all() -> Result<BTreeMap<String, Skill>> {
        let mut out = BTreeMap::new();
        for (name, bytes) in Self::iter() {
            let text = String::from_utf8(bytes).map_err(|e| SkillError::InvalidFieldType {
                skill: name.clone(),
                field: "<body>",
                reason: format!("SKILL.md is not valid UTF-8: {e}"),
            })?;
            out.insert(name.clone(), Skill::parse(&name, &text)?);
        }
        Ok(out)
    }
}

#[async_trait]
impl SkillSource for EmbeddedSkillSource {
    fn name(&self) -> &'static str {
        "embedded"
    }

    async fn list(&self) -> Result<Vec<SkillSummary>> {
        let mut summaries = Vec::new();
        for (name, bytes) in Self::iter() {
            let text = String::from_utf8(bytes).map_err(|e| SkillError::InvalidFieldType {
                skill: name.clone(),
                field: "<body>",
                reason: format!("SKILL.md is not valid UTF-8: {e}"),
            })?;
            let skill = Skill::parse(&name, &text)?;
            summaries.push(SkillSummary::from(&skill));
        }
        summaries.sort_by(|a, b| (a.category, &a.name).cmp(&(b.category, &b.name)));
        Ok(summaries)
    }

    async fn load(&self, name: &str) -> Result<Skill> {
        Self::load_skill(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn list_returns_empty_or_valid_before_any_skills_ship() {
        // Until the first baseline skill lands under `skills/`, the
        // embedder has nothing to produce. Either way, `list` must not
        // error — it just returns an empty vec or a set of valid
        // summaries.
        let source = EmbeddedSkillSource::new();
        let summaries = source.list().await.expect("list should not fail");
        for s in &summaries {
            assert!(!s.name.is_empty());
            assert!(!s.description.is_empty());
        }
    }

    #[tokio::test]
    async fn load_missing_skill_returns_not_found() {
        let source = EmbeddedSkillSource::new();
        let err = source.load("does-not-exist").await.unwrap_err();
        assert!(
            matches!(
                err,
                SkillError::NotFound {
                    source_name: "embedded",
                    ..
                }
            ),
            "expected NotFound(embedded), got {err:?}"
        );
    }

    #[test]
    fn source_name_is_embedded() {
        let source = EmbeddedSkillSource::new();
        assert_eq!(source.name(), "embedded");
    }

    #[test]
    fn all_returns_every_embedded_skill() {
        // `all()` should match `iter()` in shape: every entry parses as
        // a Skill with a non-empty name and a category consistent with
        // the on-disk folder tree (`skills/<NN-category>/<skill>/SKILL.md`).
        let all = EmbeddedSkillSource::all().expect("embedded skills parse");
        for (name, skill) in &all {
            assert_eq!(name, skill.name(), "map key matches skill name");
            assert!(!skill.frontmatter.description.is_empty());
            assert!(skill.version() > 0);
        }
        // `list()` covers every skill `all()` does, just as summaries.
        let summaries = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(EmbeddedSkillSource::new().list())
            .unwrap();
        assert_eq!(summaries.len(), all.len());
    }

    #[tokio::test]
    async fn load_returns_parsed_skill_for_known_name() {
        // Pick whichever skill `all()` produces first — avoids tying the
        // test to a specific skill that may be renamed later.
        let all = EmbeddedSkillSource::all().unwrap();
        let Some((name, _)) = all.iter().next() else {
            // If the catalogue is empty for some build, the earlier
            // `list_returns_empty_or_valid_before_any_skills_ship`
            // test already covers that case.
            return;
        };
        let source = EmbeddedSkillSource::new();
        let skill = source.load(name).await.expect("known skill loads");
        assert_eq!(skill.name(), name);
    }

    #[tokio::test]
    async fn list_is_sorted_by_category_then_name() {
        let source = EmbeddedSkillSource::new();
        let summaries = source.list().await.unwrap();
        for pair in summaries.windows(2) {
            let (a, b) = (&pair[0], &pair[1]);
            let ord = (a.category, a.name.as_str()).cmp(&(b.category, b.name.as_str()));
            assert!(
                ord == std::cmp::Ordering::Less || ord == std::cmp::Ordering::Equal,
                "summaries not sorted: {} then {}",
                a.name,
                b.name
            );
        }
    }
}
