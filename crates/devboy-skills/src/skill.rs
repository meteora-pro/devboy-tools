//! Skill, frontmatter parsing, and category enumeration.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

use crate::error::{Result, SkillError};

/// One of the six baseline skill categories. Keep in sync with the
/// on-disk `skills/<NN-category>/` layout and with [`Self::from_str`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Category {
    /// `skills/00-self-bootstrap/` — skills that configure or repair
    /// devboy itself.
    SelfBootstrap,
    /// `skills/01-issue-tracking/` — skills that operate on issues.
    IssueTracking,
    /// `skills/02-code-review/` — skills that operate on merge / pull
    /// requests.
    CodeReview,
    /// `skills/03-self-feedback/` — skills that read session traces to
    /// produce retros, daily reports, knowledge extracts.
    SelfFeedback,
    /// `skills/04-meeting-notes/` — skills that work with meeting
    /// transcripts and summaries.
    MeetingNotes,
    /// `skills/05-messenger/` — skills that interact with chats.
    Messenger,
}

impl Category {
    /// Return the canonical kebab-case identifier used on disk and in the
    /// frontmatter.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::SelfBootstrap => "self-bootstrap",
            Self::IssueTracking => "issue-tracking",
            Self::CodeReview => "code-review",
            Self::SelfFeedback => "self-feedback",
            Self::MeetingNotes => "meeting-notes",
            Self::Messenger => "messenger",
        }
    }

    /// Iterate over every known category in the order categories appear
    /// in the on-disk directory tree.
    pub fn all() -> &'static [Category] {
        &[
            Self::SelfBootstrap,
            Self::IssueTracking,
            Self::CodeReview,
            Self::SelfFeedback,
            Self::MeetingNotes,
            Self::Messenger,
        ]
    }

    /// Parse a string identifier into a category. Accepts both the
    /// kebab-case canonical form (`self-bootstrap`) and the numeric-prefix
    /// directory form (`00-self-bootstrap`).
    pub fn parse(s: &str) -> Option<Self> {
        let trimmed = s.trim_start_matches(|c: char| c.is_ascii_digit() || c == '-');
        match trimmed {
            "self-bootstrap" => Some(Self::SelfBootstrap),
            "issue-tracking" => Some(Self::IssueTracking),
            "code-review" => Some(Self::CodeReview),
            "self-feedback" => Some(Self::SelfFeedback),
            "meeting-notes" => Some(Self::MeetingNotes),
            "messenger" => Some(Self::Messenger),
            _ => None,
        }
    }
}

impl fmt::Display for Category {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Metadata extracted from the YAML frontmatter of a `SKILL.md` file.
///
/// Required fields are modelled as strongly-typed values; unknown fields
/// are preserved in [`Self::extra`] so agent-specific extensions (custom
/// activation rules, capability declarations, …) are not silently dropped.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Frontmatter {
    /// Skill name. Must match the containing directory.
    pub name: String,
    /// One-line description used by `devboy skills list`.
    pub description: String,
    /// Category that drives filtering and the install-layout path.
    pub category: Category,
    /// Integer version bumped on every change.
    pub version: u32,
    /// Optional semver range against the tool bundle (e.g.
    /// `"devboy-tools >= 0.18"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compatibility: Option<String>,
    /// Optional list of trigger phrases for agents that support
    /// activation-based skill invocation.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub activation: Vec<String>,
    /// Optional list of tools the skill calls. Used by the future
    /// compatibility checker described in ADR-014.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<String>,
    /// Any frontmatter fields we did not explicitly model. Preserved so
    /// agent-specific keys survive a round-trip.
    #[serde(flatten)]
    pub extra: BTreeMap<String, JsonValue>,
}

/// A fully-loaded skill: parsed frontmatter plus the Markdown body.
///
/// Constructing a `Skill` runs the validation in [`Skill::parse`]; the
/// public constructors either produce a valid instance or a
/// [`SkillError`].
#[derive(Debug, Clone)]
pub struct Skill {
    /// Parsed frontmatter.
    pub frontmatter: Frontmatter,
    /// Markdown body (everything after the closing `---` delimiter).
    pub body: String,
}

impl Skill {
    /// Parse a full `SKILL.md` payload. Returns [`SkillError`] for any
    /// frontmatter-level problem; callers should supply the skill's
    /// directory name as `skill_id` so error messages can point at the
    /// right place on disk.
    pub fn parse(skill_id: &str, contents: &str) -> Result<Self> {
        let (raw_yaml, body) = split_frontmatter(skill_id, contents)?;

        // Parse into a raw serde_yaml::Value first so we can surface
        // missing-required-field errors with the canonical name rather
        // than whatever serde's derive happens to use.
        let yaml: serde_yaml::Value =
            serde_yaml::from_str(raw_yaml).map_err(|source| SkillError::InvalidYaml {
                skill: skill_id.to_string(),
                source,
            })?;

        let mapping = yaml
            .as_mapping()
            .ok_or_else(|| SkillError::InvalidFieldType {
                skill: skill_id.to_string(),
                field: "<root>",
                reason: "frontmatter must be a YAML mapping".into(),
            })?;

        require_string(mapping, skill_id, "name")?;
        require_string(mapping, skill_id, "description")?;
        require_string(mapping, skill_id, "category")?;
        require_u32(mapping, skill_id, "version")?;

        // Validate the category explicitly so we produce a nice
        // UnknownCategory error rather than a generic deserialise error.
        let category_str = mapping
            .get(serde_yaml::Value::String("category".into()))
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        if Category::parse(category_str).is_none() {
            return Err(SkillError::UnknownCategory {
                skill: skill_id.to_string(),
                category: category_str.to_string(),
            });
        }

        let frontmatter: Frontmatter =
            serde_yaml::from_value(yaml).map_err(|source| SkillError::InvalidYaml {
                skill: skill_id.to_string(),
                source,
            })?;

        // The frontmatter `name` field is documented as "must match the
        // containing directory". Enforce that here so the expectation is
        // actually checked rather than being a suggestion.
        if frontmatter.name != skill_id {
            return Err(SkillError::InvalidFieldType {
                skill: skill_id.to_string(),
                field: "name",
                reason: format!(
                    "must match the containing directory `{skill_id}`, got `{}`",
                    frontmatter.name
                ),
            });
        }

        Ok(Self {
            frontmatter,
            body: body.to_string(),
        })
    }

    /// Convenience accessor for the skill's name.
    pub fn name(&self) -> &str {
        &self.frontmatter.name
    }

    /// Convenience accessor for the skill's category.
    pub fn category(&self) -> Category {
        self.frontmatter.category
    }

    /// Convenience accessor for the skill's version.
    pub fn version(&self) -> u32 {
        self.frontmatter.version
    }
}

/// Lightweight summary used by `devboy skills list` — avoids loading the
/// full body when a source only needs the names and descriptions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillSummary {
    /// Skill name.
    pub name: String,
    /// Category.
    pub category: Category,
    /// Integer version.
    pub version: u32,
    /// One-line description.
    pub description: String,
}

impl From<&Skill> for SkillSummary {
    fn from(skill: &Skill) -> Self {
        Self {
            name: skill.frontmatter.name.clone(),
            category: skill.frontmatter.category,
            version: skill.frontmatter.version,
            description: skill.frontmatter.description.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// internals
// ---------------------------------------------------------------------------

fn split_frontmatter<'a>(skill_id: &str, contents: &'a str) -> Result<(&'a str, &'a str)> {
    let contents = contents.strip_prefix('\u{FEFF}').unwrap_or(contents);

    let rest = contents
        .strip_prefix("---")
        .ok_or_else(|| SkillError::MissingFrontmatter {
            skill: skill_id.to_string(),
        })?;
    // The opening fence line can have trailing whitespace or a newline.
    let rest = rest.trim_start_matches('\r');
    let rest = rest.strip_prefix('\n').unwrap_or(rest);

    // Find the closing fence — must sit on its own line.
    let close_idx =
        find_line_starting_with(rest, "---").ok_or_else(|| SkillError::MissingFrontmatter {
            skill: skill_id.to_string(),
        })?;

    let yaml = &rest[..close_idx];
    let after_close = &rest[close_idx..];
    // Skip the closing `---` line itself (and any `\r\n`).
    let body = match after_close.find('\n') {
        Some(idx) => &after_close[idx + 1..],
        None => "",
    };

    Ok((yaml, body))
}

fn find_line_starting_with(haystack: &str, needle: &str) -> Option<usize> {
    let mut idx = 0usize;
    for line in haystack.split_inclusive('\n') {
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed == needle {
            return Some(idx);
        }
        idx += line.len();
    }
    None
}

fn require_string(mapping: &serde_yaml::Mapping, skill: &str, field: &'static str) -> Result<()> {
    let value = mapping.get(serde_yaml::Value::String(field.into()));
    match value {
        None | Some(serde_yaml::Value::Null) => Err(SkillError::MissingRequiredField {
            skill: skill.to_string(),
            field,
        }),
        Some(v) if v.as_str().is_some() => Ok(()),
        Some(_) => Err(SkillError::InvalidFieldType {
            skill: skill.to_string(),
            field,
            reason: "expected a string".into(),
        }),
    }
}

fn require_u32(mapping: &serde_yaml::Mapping, skill: &str, field: &'static str) -> Result<()> {
    let value = mapping.get(serde_yaml::Value::String(field.into()));
    match value {
        None | Some(serde_yaml::Value::Null) => Err(SkillError::MissingRequiredField {
            skill: skill.to_string(),
            field,
        }),
        Some(v) => match v.as_u64() {
            Some(n) if n <= u64::from(u32::MAX) => Ok(()),
            _ => Err(SkillError::InvalidFieldType {
                skill: skill.to_string(),
                field,
                reason: "expected a non-negative integer that fits in u32".into(),
            }),
        },
    }
}

// ---------------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const VALID: &str = r#"---
name: setup
description: Walk the user through initial devboy configuration.
category: self-bootstrap
version: 1
compatibility: devboy-tools >= 0.18
activation:
  - "configure devboy"
  - "setup devboy"
tools:
  - doctor
  - config
---

# setup

Body goes here.
"#;

    #[test]
    fn parses_valid_skill() {
        let skill = Skill::parse("setup", VALID).expect("valid skill parses");
        assert_eq!(skill.name(), "setup");
        assert_eq!(skill.category(), Category::SelfBootstrap);
        assert_eq!(skill.version(), 1);
        assert_eq!(skill.frontmatter.activation.len(), 2);
        assert!(skill.body.contains("Body goes here"));
    }

    #[test]
    fn rejects_missing_frontmatter() {
        let input = "no frontmatter here\n";
        let err = Skill::parse("foo", input).unwrap_err();
        assert!(matches!(err, SkillError::MissingFrontmatter { .. }));
    }

    #[test]
    fn rejects_missing_required_field() {
        let input = r#"---
name: setup
description: test
category: self-bootstrap
---
body
"#;
        let err = Skill::parse("setup", input).unwrap_err();
        assert!(
            matches!(
                err,
                SkillError::MissingRequiredField {
                    field: "version",
                    ..
                }
            ),
            "expected MissingRequiredField(version), got {err:?}"
        );
    }

    #[test]
    fn rejects_wrong_field_type() {
        let input = r#"---
name: setup
description: test
category: self-bootstrap
version: "not a number"
---
body
"#;
        let err = Skill::parse("setup", input).unwrap_err();
        assert!(
            matches!(
                err,
                SkillError::InvalidFieldType {
                    field: "version",
                    ..
                }
            ),
            "expected InvalidFieldType(version), got {err:?}"
        );
    }

    #[test]
    fn rejects_unknown_category() {
        let input = r#"---
name: setup
description: test
category: not-a-real-category
version: 1
---
body
"#;
        let err = Skill::parse("setup", input).unwrap_err();
        assert!(
            matches!(err, SkillError::UnknownCategory { ref category, .. } if category == "not-a-real-category"),
            "expected UnknownCategory, got {err:?}"
        );
    }

    #[test]
    fn preserves_unknown_frontmatter_fields() {
        let input = r#"---
name: setup
description: test
category: self-bootstrap
version: 1
x-custom-vendor-field: hello
---
body
"#;
        let skill = Skill::parse("setup", input).unwrap();
        assert!(
            skill
                .frontmatter
                .extra
                .contains_key("x-custom-vendor-field")
        );
    }

    #[test]
    fn category_round_trip() {
        for cat in Category::all() {
            let parsed = Category::parse(cat.as_str()).unwrap();
            assert_eq!(parsed, *cat);
        }
        assert_eq!(
            Category::parse("00-self-bootstrap"),
            Some(Category::SelfBootstrap)
        );
        assert!(Category::parse("not-real").is_none());
    }

    #[test]
    fn summary_from_skill() {
        let skill = Skill::parse("setup", VALID).unwrap();
        let sum = SkillSummary::from(&skill);
        assert_eq!(sum.name, "setup");
        assert_eq!(sum.category, Category::SelfBootstrap);
    }

    #[test]
    fn category_as_str_matches_parse() {
        for cat in Category::all() {
            // Display == as_str and both round-trip through parse.
            assert_eq!(cat.as_str(), format!("{}", cat));
            assert_eq!(Category::parse(cat.as_str()), Some(*cat));
        }
    }

    #[test]
    fn parse_accepts_numeric_prefix_directory_form() {
        // The filesystem layout uses `<NN>-<category>/` directory
        // names (`01-issue-tracking`, `05-messenger`, …). The parser
        // has to accept those too.
        assert_eq!(
            Category::parse("01-issue-tracking"),
            Some(Category::IssueTracking)
        );
        assert_eq!(Category::parse("05-messenger"), Some(Category::Messenger));
        // Double-digit-only prefix is also accepted (harmless).
        assert_eq!(
            Category::parse("42-self-bootstrap"),
            Some(Category::SelfBootstrap)
        );
    }

    #[test]
    fn parse_rejects_frontmatter_without_closing_fence() {
        // Missing closing `---` must yield MissingFrontmatter, not
        // a YAML error on the half-parsed body.
        let input = "---\nname: setup\ndescription: incomplete\ncategory: self-bootstrap\nversion: 1\nbody starts here\n";
        let err = Skill::parse("setup", input).unwrap_err();
        assert!(
            matches!(err, SkillError::MissingFrontmatter { .. }),
            "expected MissingFrontmatter, got {err:?}"
        );
    }

    #[test]
    fn parse_strips_bom_if_present() {
        // UTF-8 BOM at the very start must not confuse the fence
        // detector.
        let bom_input = format!("\u{FEFF}{VALID}");
        let skill = Skill::parse("setup", &bom_input).expect("BOM-prefixed file parses");
        assert_eq!(skill.name(), "setup");
    }

    #[test]
    fn parse_rejects_non_mapping_frontmatter() {
        // Frontmatter that is valid YAML but not a mapping (list,
        // scalar, etc.) must error out.
        let input = "---\n- list-not-mapping\n- another\n---\nbody\n";
        let err = Skill::parse("whatever", input).unwrap_err();
        assert!(
            matches!(
                err,
                SkillError::InvalidFieldType {
                    field: "<root>",
                    ..
                }
            ),
            "expected InvalidFieldType(<root>), got {err:?}"
        );
    }

    #[test]
    fn parse_rejects_mismatched_name_and_skill_id() {
        // `frontmatter.name` is contractually "must match the
        // containing directory"; enforce by constructing a payload
        // where the two disagree.
        let input = r#"---
name: wrong-name
description: test
category: self-bootstrap
version: 1
---
body
"#;
        let err = Skill::parse("setup", input).unwrap_err();
        assert!(
            matches!(err, SkillError::InvalidFieldType { field: "name", .. }),
            "expected InvalidFieldType(name), got {err:?}"
        );
    }

    #[test]
    fn parse_rejects_negative_version() {
        let input = r#"---
name: setup
description: test
category: self-bootstrap
version: -1
---
body
"#;
        let err = Skill::parse("setup", input).unwrap_err();
        // Negative values serialise as i64 in serde_yaml, so they
        // fail the `as_u64()` guard in `require_u32`.
        assert!(
            matches!(
                err,
                SkillError::InvalidFieldType {
                    field: "version",
                    ..
                }
            ),
            "expected InvalidFieldType(version), got {err:?}"
        );
    }
}
