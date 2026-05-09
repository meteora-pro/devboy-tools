//! Per-project secret manifest per [ADR-020] §4.
//!
//! A project that uses `devboy-tools` declares its dependency on
//! secrets in `.devboy/secrets.toml` checked into the repository.
//! Three categories of declarations are recognised:
//!
//! 1. `required` and `optional` — *references* into the secret
//!    namespace. `doctor` fails the exit code on a missing required
//!    path; missing optional paths surface as informational.
//! 2. `[overrides."<path>"]` — behavioural overrides applied on top
//!    of the global-index entry for that path. Only three fields
//!    may be overridden (`gate`, `rotate_every_days`, `description`);
//!    attempts to override anything else are rejected at parse time
//!    with `deny_unknown_fields` so drift between project and
//!    global cannot grow silently.
//! 3. `[secret."<path>"]` — *project-local* metadata for a path that
//!    does not exist in the global index. The loader treats such a
//!    path as if its global entry were absent (the merge logic in
//!    P1.4 reads from the manifest exclusively for these paths).
//!
//! # File layout
//!
//! ```toml
//! # .devboy/secrets.toml
//!
//! required = [
//!     "team/gitlab/token-deploy",
//!     "personal/github/pat",
//! ]
//!
//! optional = ["personal/slack/notify-token"]
//!
//! [overrides."team/gitlab/token-deploy"]
//! gate              = "touchid"
//! rotate_every_days = 30
//! description       = "Used by the staging deploy pipeline"
//!
//! [secret."sandbox/example-provider/token"]
//! description   = "Sandbox-only; recreated per-developer"
//! retrieval_url = "https://example-provider.dev/account/api-tokens"
//! pattern_id    = "generic-bearer"
//! ```
//!
//! # Path validation
//!
//! Every path that appears in any of the four positions is parsed
//! through [`SecretPath::parse`] at load time. Invalid paths produce
//! [`ManifestError::Path`] with a [`PathRole`] tag identifying *which*
//! position the bad path appeared in, so error messages can point at
//! the right TOML location.
//!
//! [ADR-020]: https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-020-secret-manifest-and-alias-resolution.md

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::debug;

use crate::index::{Gate, IndexEntry};
use crate::secret_path::{PathError, SecretPath};

/// Conventional path of the per-project manifest, relative to the
/// project root.
pub const MANIFEST_RELATIVE_PATH: &str = ".devboy/secrets.toml";

/// Position in the manifest where a bad path was encountered.
///
/// Surfaced through [`ManifestError::Path`] so the user sees *where*
/// to look in the TOML file, not just *what* the violation was.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathRole {
    /// Inside the top-level `required = [...]` list.
    Required,
    /// Inside the top-level `optional = [...]` list.
    Optional,
    /// Key of an `[overrides."<path>"]` table.
    OverrideKey,
    /// Key of a `[secret."<path>"]` table.
    SecretKey,
}

impl fmt::Display for PathRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Required => "required[]",
            Self::Optional => "optional[]",
            Self::OverrideKey => "[overrides.\"...\"]",
            Self::SecretKey => "[secret.\"...\"]",
        };
        f.write_str(s)
    }
}

/// Failure modes when loading or parsing a [`ProjectManifest`].
#[derive(Debug, Error)]
pub enum ManifestError {
    /// I/O error reading the manifest file.
    #[error("failed to read project manifest at {path}: {source}")]
    Read {
        /// File the loader tried to read.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },

    /// TOML deserialization error. Triggered by syntax errors and by
    /// `deny_unknown_fields` violations such as putting `format_regex`
    /// inside an `[overrides."..."]` block.
    #[error("failed to parse project manifest at {path}: {source}")]
    Parse {
        /// File the parser tried to read.
        path: PathBuf,
        /// Underlying TOML deserialization error.
        #[source]
        source: toml::de::Error,
    },

    /// One of the paths in the manifest did not satisfy the path
    /// convention from ADR-020 §2.
    #[error("invalid path in manifest position {role}: {source}")]
    Path {
        /// The position in which the bad path appeared.
        role: PathRole,
        /// Underlying path-validation error.
        #[source]
        source: PathError,
    },
}

/// Behavioural override applied to a path whose canonical metadata
/// lives in the global index.
///
/// Per ADR-020 §4, **only** `gate`, `rotate_every_days`, and
/// `description` may be overridden — everything else (regex, retrieval
/// URL, rotation method, …) is read from the global index only. The
/// `deny_unknown_fields` attribute enforces this: writing
/// `retrieval_url` here is a parse error rather than a silently-ignored
/// no-op.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OverrideEntry {
    /// Tightens the default gate from the global index for this
    /// project.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gate: Option<Gate>,

    /// Project-specific rotation cadence in days. Tightens whatever
    /// the global index recommends.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotate_every_days: Option<u32>,

    /// Project-local note that appears alongside the global
    /// description in `secrets describe` and the inventory view
    /// (e.g. "in this repo this is the staging deploy token").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl OverrideEntry {
    /// `true` when no field is set — useful for collapsing the
    /// no-op-override warning (`doctor` flags blocks where every
    /// override matches the global value).
    pub fn is_empty(&self) -> bool {
        self.gate.is_none() && self.rotate_every_days.is_none() && self.description.is_none()
    }
}

/// In-memory representation of `.devboy/secrets.toml`.
///
/// This loader does **not** merge with the global index — it is the
/// raw view of what the project committed. The merge logic (precedence
/// rules, `E_OVERRIDE_FIELD_NOT_ALLOWED` for non-allowed fields,
/// `E_SECRET_UNKNOWN_PATH` for paths missing from both global and
/// project-local) lands in epic phase P1.4 (#18).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectManifest {
    /// Paths the project requires to function. `doctor` returns
    /// non-zero if any are missing.
    pub required: Vec<SecretPath>,

    /// Paths the project can use but does not require.
    pub optional: Vec<SecretPath>,

    /// Behavioural overrides keyed by path.
    pub overrides: BTreeMap<SecretPath, OverrideEntry>,

    /// Project-local entries — paths that do not appear in the global
    /// index and whose metadata lives in this manifest.
    pub secrets: BTreeMap<SecretPath, IndexEntry>,
}

/// Internal serde shape — every key is a raw `String` so the loader
/// can convert to `SecretPath` with role-tagged error reporting.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RawManifest {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    required: Vec<String>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    optional: Vec<String>,

    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    overrides: BTreeMap<String, OverrideEntry>,

    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    secret: BTreeMap<String, IndexEntry>,
}

impl ProjectManifest {
    /// Build an empty manifest. Useful for tests and as the default
    /// when `.devboy/secrets.toml` is absent (the manifest is opt-in
    /// per ADR-020 §4).
    pub fn new() -> Self {
        Self::default()
    }

    /// Load the manifest from `<cwd>/.devboy/secrets.toml`. Returns
    /// an empty manifest if the file does not exist.
    pub fn load() -> Result<Self, ManifestError> {
        let cwd = std::env::current_dir().map_err(|e| ManifestError::Read {
            path: PathBuf::from(MANIFEST_RELATIVE_PATH),
            source: e,
        })?;
        Self::load_from_project_root(&cwd)
    }

    /// Load the manifest from `<project_root>/.devboy/secrets.toml`.
    pub fn load_from_project_root(root: &Path) -> Result<Self, ManifestError> {
        Self::load_from(&root.join(MANIFEST_RELATIVE_PATH))
    }

    /// Load the manifest from a specific file path. Returns an empty
    /// manifest if the file does not exist.
    pub fn load_from(path: &Path) -> Result<Self, ManifestError> {
        if !path.exists() {
            debug!(path = ?path, "project manifest not present, using empty");
            return Ok(Self::new());
        }
        let body = fs::read_to_string(path).map_err(|e| ManifestError::Read {
            path: path.to_path_buf(),
            source: e,
        })?;
        Self::from_str_with_path(&body, path)
    }

    /// Parse from a TOML string with no associated file path (used in
    /// tests and when the caller has bytes in memory).
    pub fn from_toml_str(body: &str) -> Result<Self, ManifestError> {
        Self::from_str_with_path(body, Path::new("<inline>"))
    }

    fn from_str_with_path(body: &str, path: &Path) -> Result<Self, ManifestError> {
        let raw: RawManifest = toml::from_str(body).map_err(|e| ManifestError::Parse {
            path: path.to_path_buf(),
            source: e,
        })?;

        let required = parse_path_list(&raw.required, PathRole::Required)?;
        let optional = parse_path_list(&raw.optional, PathRole::Optional)?;
        let overrides = parse_path_map(raw.overrides, PathRole::OverrideKey)?;
        let secrets = parse_path_map(raw.secret, PathRole::SecretKey)?;

        Ok(Self {
            required,
            optional,
            overrides,
            secrets,
        })
    }

    /// `true` when no required, optional, override, or project-local
    /// entry is declared. `doctor` treats this as "the project does
    /// not currently depend on any managed secret".
    pub fn is_empty(&self) -> bool {
        self.required.is_empty()
            && self.optional.is_empty()
            && self.overrides.is_empty()
            && self.secrets.is_empty()
    }

    /// Iterate over every path mentioned by the manifest in any role
    /// (required, optional, override key, project-local secret key),
    /// each tagged with the role it appeared in. Useful for `doctor`
    /// and the merge logic.
    pub fn referenced_paths(&self) -> impl Iterator<Item = (&SecretPath, PathRole)> {
        self.required
            .iter()
            .map(|p| (p, PathRole::Required))
            .chain(self.optional.iter().map(|p| (p, PathRole::Optional)))
            .chain(self.overrides.keys().map(|p| (p, PathRole::OverrideKey)))
            .chain(self.secrets.keys().map(|p| (p, PathRole::SecretKey)))
    }
}

fn parse_path_list(raw: &[String], role: PathRole) -> Result<Vec<SecretPath>, ManifestError> {
    raw.iter()
        .map(|s| SecretPath::parse(s).map_err(|source| ManifestError::Path { role, source }))
        .collect()
}

fn parse_path_map<V>(
    raw: BTreeMap<String, V>,
    role: PathRole,
) -> Result<BTreeMap<SecretPath, V>, ManifestError> {
    let mut out = BTreeMap::new();
    for (k, v) in raw {
        let p = SecretPath::parse(&k).map_err(|source| ManifestError::Path { role, source })?;
        out.insert(p, v);
    }
    Ok(out)
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::RotationMethod;

    fn fixture_full_manifest() -> &'static str {
        r#"
required = [
    "team/gitlab/token-deploy",
    "personal/github/pat",
]

optional = ["personal/slack/notify-token"]

[overrides."team/gitlab/token-deploy"]
gate              = "touchid"
rotate_every_days = 30
description       = "Used by the staging deploy pipeline"

[secret."sandbox/example-provider/token"]
description   = "Sandbox-only token recreated per developer"
retrieval_url = "https://example-provider.dev/account/api-tokens"
pattern_id    = "generic-bearer"
rotation_method = "manual"
"#
    }

    #[test]
    fn empty_string_yields_empty_manifest() {
        let m = ProjectManifest::from_toml_str("").unwrap();
        assert!(m.is_empty());
        assert!(m.required.is_empty());
        assert!(m.optional.is_empty());
        assert!(m.overrides.is_empty());
        assert!(m.secrets.is_empty());
    }

    #[test]
    fn parses_full_manifest() {
        let m = ProjectManifest::from_toml_str(fixture_full_manifest()).unwrap();

        assert_eq!(m.required.len(), 2);
        assert_eq!(m.required[0].as_str(), "team/gitlab/token-deploy");
        assert_eq!(m.required[1].as_str(), "personal/github/pat");

        assert_eq!(m.optional.len(), 1);
        assert_eq!(m.optional[0].as_str(), "personal/slack/notify-token");

        let override_path: SecretPath = "team/gitlab/token-deploy".parse().unwrap();
        let ov = m.overrides.get(&override_path).expect("override present");
        assert_eq!(ov.gate, Some(Gate::Touchid));
        assert_eq!(ov.rotate_every_days, Some(30));
        assert_eq!(
            ov.description.as_deref(),
            Some("Used by the staging deploy pipeline")
        );

        let local_path: SecretPath = "sandbox/example-provider/token".parse().unwrap();
        let sec = m.secrets.get(&local_path).expect("project-local present");
        assert_eq!(
            sec.description.as_deref(),
            Some("Sandbox-only token recreated per developer")
        );
        assert_eq!(
            sec.retrieval_url.as_deref(),
            Some("https://example-provider.dev/account/api-tokens")
        );
        assert_eq!(sec.pattern_id.as_deref(), Some("generic-bearer"));
        assert_eq!(sec.rotation_method, Some(RotationMethod::Manual));
    }

    #[test]
    fn parses_only_required() {
        let m = ProjectManifest::from_toml_str(
            r#"
required = ["team/gitlab/token-deploy"]
"#,
        )
        .unwrap();
        assert_eq!(m.required.len(), 1);
        assert!(m.optional.is_empty());
        assert!(m.overrides.is_empty());
        assert!(m.secrets.is_empty());
        assert!(!m.is_empty());
    }

    #[test]
    fn parses_only_overrides() {
        let m = ProjectManifest::from_toml_str(
            r#"
[overrides."team/gitlab/token-deploy"]
gate = "confirm"
"#,
        )
        .unwrap();
        let p: SecretPath = "team/gitlab/token-deploy".parse().unwrap();
        assert_eq!(m.overrides.get(&p).unwrap().gate, Some(Gate::Confirm));
    }

    // -- Path validation -------------------------------------------------------

    #[test]
    fn rejects_invalid_path_in_required() {
        let err = ProjectManifest::from_toml_str(
            r#"
required = ["gitlab.token"]
"#,
        )
        .unwrap_err();
        match err {
            ManifestError::Path { role, source } => {
                assert_eq!(role, PathRole::Required);
                assert!(matches!(source, PathError::TooFewSegments { .. }));
            }
            other => panic!("expected Path error, got {other:?}"),
        }
    }

    #[test]
    fn rejects_invalid_path_in_optional() {
        let err = ProjectManifest::from_toml_str(
            r#"
optional = ["Bad/Path/Format"]
"#,
        )
        .unwrap_err();
        match err {
            ManifestError::Path { role, source } => {
                assert_eq!(role, PathRole::Optional);
                assert!(matches!(source, PathError::BadSegment { .. }));
            }
            other => panic!("expected Path error, got {other:?}"),
        }
    }

    #[test]
    fn rejects_invalid_path_in_override_key() {
        let err = ProjectManifest::from_toml_str(
            r#"
[overrides."team/gitlab"]
gate = "auto"
"#,
        )
        .unwrap_err();
        match err {
            ManifestError::Path { role, source } => {
                assert_eq!(role, PathRole::OverrideKey);
                assert!(matches!(source, PathError::TooFewSegments { found: 2, .. }));
            }
            other => panic!("expected Path error, got {other:?}"),
        }
    }

    #[test]
    fn rejects_invalid_path_in_secret_key() {
        let err = ProjectManifest::from_toml_str(
            r#"
[secret."__sources/vault/token"]
description = "internal"
"#,
        )
        .unwrap_err();
        match err {
            ManifestError::Path { role, source } => {
                assert_eq!(role, PathRole::SecretKey);
                assert!(matches!(source, PathError::ReservedPrefix { .. }));
            }
            other => panic!("expected Path error, got {other:?}"),
        }
    }

    // -- Override field validation --------------------------------------------

    #[test]
    fn rejects_disallowed_override_field() {
        // ADR-020 §4: only behavioural fields may live in [overrides].
        // Non-behavioural metadata (format_regex, retrieval_url, rotation_method,
        // pattern_id, ...) must come from the global index only.
        for bad_field in [
            "format_regex = \"^x$\"",
            "retrieval_url = \"https://example.com\"",
            "rotation_method = \"manual\"",
            "pattern_id = \"x\"",
            "expires_at = \"2026-08-01\"",
        ] {
            let toml = format!("[overrides.\"team/gitlab/token-deploy\"]\n{bad_field}\n");
            let err = ProjectManifest::from_toml_str(&toml).unwrap_err();
            assert!(
                matches!(err, ManifestError::Parse { .. }),
                "field {bad_field:?} should be rejected at parse time, got {err:?}"
            );
        }
    }

    #[test]
    fn override_entry_is_empty_when_all_fields_missing() {
        let m = ProjectManifest::from_toml_str(
            r#"
[overrides."team/gitlab/token-deploy"]
"#,
        )
        .unwrap();
        let p: SecretPath = "team/gitlab/token-deploy".parse().unwrap();
        let ov = m.overrides.get(&p).unwrap();
        assert!(ov.is_empty());
    }

    #[test]
    fn override_entry_is_not_empty_when_any_field_set() {
        let m = ProjectManifest::from_toml_str(
            r#"
[overrides."team/gitlab/token-deploy"]
gate = "auto"
"#,
        )
        .unwrap();
        let p: SecretPath = "team/gitlab/token-deploy".parse().unwrap();
        assert!(!m.overrides.get(&p).unwrap().is_empty());
    }

    // -- Top-level unknown-field rejection ------------------------------------

    #[test]
    fn rejects_unknown_top_level_field() {
        let err = ProjectManifest::from_toml_str(
            r#"
required = []
extra_field = "something"
"#,
        )
        .unwrap_err();
        assert!(matches!(err, ManifestError::Parse { .. }));
    }

    // -- File-loading paths ---------------------------------------------------

    #[test]
    fn load_from_returns_empty_when_file_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing.toml");
        let m = ProjectManifest::load_from(&path).unwrap();
        assert!(m.is_empty());
    }

    #[test]
    fn load_from_real_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.toml");
        std::fs::write(&path, fixture_full_manifest()).unwrap();
        let m = ProjectManifest::load_from(&path).unwrap();
        assert_eq!(m.required.len(), 2);
        assert_eq!(m.secrets.len(), 1);
    }

    #[test]
    fn load_from_project_root_resolves_dot_devboy_path() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".devboy")).unwrap();
        let manifest_path = dir.path().join(MANIFEST_RELATIVE_PATH);
        std::fs::write(&manifest_path, fixture_full_manifest()).unwrap();
        let m = ProjectManifest::load_from_project_root(dir.path()).unwrap();
        assert_eq!(m.required.len(), 2);
    }

    #[test]
    fn load_from_returns_empty_when_dot_devboy_dir_missing() {
        let dir = tempfile::tempdir().unwrap();
        let m = ProjectManifest::load_from_project_root(dir.path()).unwrap();
        assert!(m.is_empty());
    }

    #[test]
    fn load_io_error_surfaces_path() {
        let dir = tempfile::tempdir().unwrap();
        let err = ProjectManifest::load_from(dir.path()).unwrap_err();
        match err {
            ManifestError::Read { path, .. } => assert_eq!(path, dir.path()),
            other => panic!("expected Read, got {other:?}"),
        }
    }

    // -- referenced_paths iterator --------------------------------------------

    #[test]
    fn referenced_paths_yields_each_role() {
        let m = ProjectManifest::from_toml_str(fixture_full_manifest()).unwrap();
        let paths: Vec<(String, PathRole)> = m
            .referenced_paths()
            .map(|(p, r)| (p.as_str().to_owned(), r))
            .collect();

        // Two required, one optional, one override key, one project-local key.
        assert_eq!(paths.len(), 5);
        assert!(
            paths
                .iter()
                .any(|(p, r)| p == "team/gitlab/token-deploy" && *r == PathRole::Required)
        );
        assert!(
            paths
                .iter()
                .any(|(p, r)| p == "personal/github/pat" && *r == PathRole::Required)
        );
        assert!(
            paths
                .iter()
                .any(|(p, r)| p == "personal/slack/notify-token" && *r == PathRole::Optional)
        );
        assert!(
            paths
                .iter()
                .any(|(p, r)| p == "team/gitlab/token-deploy" && *r == PathRole::OverrideKey)
        );
        assert!(
            paths
                .iter()
                .any(|(p, r)| p == "sandbox/example-provider/token" && *r == PathRole::SecretKey)
        );
    }

    #[test]
    fn path_role_display() {
        assert_eq!(PathRole::Required.to_string(), "required[]");
        assert_eq!(PathRole::Optional.to_string(), "optional[]");
        assert_eq!(PathRole::OverrideKey.to_string(), "[overrides.\"...\"]");
        assert_eq!(PathRole::SecretKey.to_string(), "[secret.\"...\"]");
    }
}
