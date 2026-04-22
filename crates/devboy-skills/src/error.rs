//! Error types for the skills subsystem.

use std::path::PathBuf;

/// Result alias used throughout `devboy-skills`.
pub type Result<T> = std::result::Result<T, SkillError>;

/// Errors that can surface while loading, parsing, or installing a skill.
#[derive(Debug, thiserror::Error)]
pub enum SkillError {
    /// The skill body did not open with a YAML frontmatter block
    /// (`---` at line 1, closing `---` on a subsequent line).
    #[error("skill `{skill}`: missing YAML frontmatter delimiter `---`")]
    MissingFrontmatter {
        /// Identifier of the offending skill (directory name).
        skill: String,
    },

    /// The YAML frontmatter parsed but did not contain a required field.
    #[error("skill `{skill}`: required frontmatter field `{field}` is missing")]
    MissingRequiredField {
        /// Identifier of the offending skill.
        skill: String,
        /// Name of the missing field.
        field: &'static str,
    },

    /// The YAML frontmatter parsed but a field had the wrong type.
    #[error("skill `{skill}`: frontmatter field `{field}` has invalid type ({reason})")]
    InvalidFieldType {
        /// Identifier of the offending skill.
        skill: String,
        /// Name of the offending field.
        field: &'static str,
        /// Why the value is rejected.
        reason: String,
    },

    /// The `category` field references a category that is not in the
    /// known enum. Provide one of the shipped categories (self-bootstrap,
    /// issue-tracking, code-review, self-feedback, meeting-notes, messenger)
    /// or extend [`crate::skill::Category`] before shipping a new one.
    #[error("skill `{skill}`: unknown category `{category}`")]
    UnknownCategory {
        /// Identifier of the offending skill.
        skill: String,
        /// The string that failed to parse as a category.
        category: String,
    },

    /// Underlying YAML parser failure (syntax error inside the frontmatter).
    #[error("skill `{skill}`: invalid YAML in frontmatter: {source}")]
    InvalidYaml {
        /// Identifier of the offending skill.
        skill: String,
        /// Wrapped YAML error.
        #[source]
        source: serde_yaml::Error,
    },

    /// A skill lookup was asked for a name that the source does not know
    /// about.
    #[error("skill `{name}` not found in source `{source_name}`")]
    NotFound {
        /// Name that was requested.
        name: String,
        /// The source that was queried.
        source_name: &'static str,
    },

    /// Filesystem operation failed while reading or writing a skill / manifest.
    #[error("filesystem error at `{path}`: {source}")]
    Io {
        /// Path that was being accessed.
        path: PathBuf,
        /// Wrapped IO error.
        #[source]
        source: std::io::Error,
    },

    /// Manifest JSON parse failure.
    #[error("manifest at `{path}`: invalid JSON ({source})")]
    InvalidManifest {
        /// Path to the offending manifest.
        path: PathBuf,
        /// Wrapped serde_json error.
        #[source]
        source: serde_json::Error,
    },
}
