//! Global secret-metadata index per [ADR-020] §3.
//!
//! The global index lives at `<config-dir>/secrets/index.toml` and
//! holds **metadata, never values**: human description, retrieval URL,
//! format regex, expiry/rotation hints, optional pattern reference.
//! It is the canonical, cross-project source of truth for everything
//! about a secret *except* the value itself.
//!
//! # File layout
//!
//! ```toml
//! [secret."team/gitlab/token-deploy"]
//! description       = "Deploy token for the team GitLab"
//! retrieval_url     = "https://gitlab.example.internal/-/profile/personal_access_tokens"
//! format_regex      = "^glpat-[A-Za-z0-9_-]{20,}$"
//! default_gate      = "auto"            # auto | confirm | touchid
//! expires_at        = "2026-08-01"      # ISO 8601 date, optional
//! last_rotated_at   = "2026-05-02"      # ISO 8601 date, optional
//! rotate_every_days = 90                # advisory, drives doctor warnings
//! rotation_method   = "manual"          # manual | provider-ui | provider-api
//! required_scopes   = ["api", "read_repository"]
//! pattern_id        = "gitlab-pat"      # devboy-secret-patterns id
//! env_var           = "GITLAB_TOKEN_DEPLOY"  # env-store override (ADR-021 §8)
//! cache_ttl_seconds_max = 60            # bound on adaptive TTL (ADR-021 §7)
//! ```
//!
//! # Path semantics
//!
//! Keys are typed as [`SecretPath`]. Loading rejects any non-conforming
//! key with [`IndexError::Path`] — a typo in a path turns into a hard
//! load-time failure, not a silent miss at lookup time.
//!
//! [ADR-020]: https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-020-secret-manifest-and-alias-resolution.md

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use devboy_core::Error as CoreError;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::debug;

use crate::secret_path::{PathError, SecretPath};

/// Subdirectory under the user's config directory that holds the
/// secret-framework configuration files (this index, the source router
/// config, the local vault file).
pub const SECRETS_SUBDIR: &str = "secrets";

/// Filename of the global metadata index inside [`SECRETS_SUBDIR`].
pub const INDEX_FILENAME: &str = "index.toml";

/// Failure modes when loading or operating on a [`GlobalIndex`].
#[derive(Debug, Error)]
pub enum IndexError {
    /// I/O error reading the index file.
    #[error("failed to read global index at {path}: {source}")]
    Read {
        /// File the loader tried to read.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },

    /// TOML deserialization error.
    #[error("failed to parse global index at {path}: {source}")]
    Parse {
        /// File the parser tried to read.
        path: PathBuf,
        /// Underlying TOML deserialization error.
        #[source]
        source: toml::de::Error,
    },

    /// TOML serialization error — surfaced by
    /// [`GlobalIndex::save_to`].
    #[error("failed to serialize global index for {path}: {source}")]
    Serialize {
        /// File the writer was about to write to.
        path: PathBuf,
        /// Underlying TOML serialization error.
        #[source]
        source: toml::ser::Error,
    },

    /// One of the keys in the index did not satisfy the path
    /// convention from ADR-020 §2.
    #[error("invalid secret path in index: {source}")]
    Path {
        /// Underlying path-validation error.
        #[source]
        source: PathError,
    },

    /// `dirs::config_dir()` returned `None` — extremely unusual on
    /// supported platforms but worth surfacing rather than panicking.
    #[error("could not resolve the user's config directory")]
    NoConfigDir,
}

impl From<IndexError> for CoreError {
    fn from(e: IndexError) -> Self {
        CoreError::Storage(e.to_string())
    }
}

/// User-controllable interaction gate for a secret.
///
/// Drives whether `secret.get` requires confirmation or biometric
/// unlock. Per-project manifests may tighten this through
/// `[overrides]` — see ADR-020 §4.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Gate {
    /// No interactive confirmation required.
    #[default]
    Auto,
    /// Show a confirmation dialog before yielding the value.
    Confirm,
    /// Require Touch ID / biometric unlock before yielding the value.
    Touchid,
}

/// How the secret is rotated.
///
/// `Manual` is the only method that ships in the first release;
/// `ProviderUi` and `ProviderApi` are reserved for future ADRs (see
/// Per-path policy for AI-agent secret USE (not provision).
///
/// Controls whether the framework demands an interactive user
/// approval each time an agent's high-level provider tool resolves
/// a `@secret:<path>` alias backed by this entry. Designed for the
/// case where the user wants to provision once but still wants
/// fine-grained control over which agent calls "spend" sensitive
/// credentials. See ADR-023 §3.7 (P25 phase) for the protocol.
///
/// Default — `Never` — preserves the existing zero-prompt
/// resolve path so most paths stay frictionless.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ApproveOnUse {
    /// No approval required — alias resolves silently. The
    /// default for every existing manifest.
    #[default]
    Never,
    /// First use this session opens an approval dialog; once
    /// the user clicks "Allow always (this session)" further
    /// uses skip the prompt for the remainder of the session.
    Session,
    /// Every use opens an approval dialog. Right for high-
    /// stakes paths (prod DB password, signing keys).
    PerCall,
}

/// ADR-023 §3.5 — provider-driven rotation is deferred).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RotationMethod {
    /// User rotates the secret themselves through the upstream UI;
    /// `devboy-tools` records the new value and validates liveness.
    #[default]
    Manual,
    /// Reserved — `devboy-tools` opens the provider's UI and accepts
    /// the new value through the rotation flow (ADR-023 §3.5 future
    /// work).
    ProviderUi,
    /// Reserved — `devboy-tools` calls the provider's rotation API
    /// directly (deferred per ADR-023 §3.5).
    ProviderApi,
}

/// Metadata for a single secret stored in the global index.
///
/// Every field is optional: missing fields fall back to defaults from
/// the linked [`pattern_id`](IndexEntry::pattern_id) (when set; that
/// inheritance is wired up in epic phase P2.4) or to the framework's
/// own defaults.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndexEntry {
    /// Free-text description shown in `secrets describe`, the UI's
    /// inventory view, and the agent's `secrets.describe` reply.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// URL the user opens to obtain a fresh value (browser link).
    /// Used by the rotation flow's `[Open URL]` button.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retrieval_url: Option<String>,

    /// Regular expression the value must match. Validation is lazy —
    /// the regex is only compiled when `secrets validate --format` or
    /// the format-validation hook runs (epic phase P9.1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format_regex: Option<String>,

    /// Confirmation gate applied at value-read time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_gate: Option<Gate>,

    /// ISO 8601 (`YYYY-MM-DD`) expiry date. Populated by the liveness
    /// validator (P9.2) when the upstream API exposes a `valid_until`
    /// field; can also be set by hand.
    ///
    /// Stored as `String` rather than a typed date so the round-trip
    /// through TOML is lossless without pulling in a date crate. The
    /// validation framework parses on demand.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,

    /// ISO 8601 date of the last successful rotation. Drives advisory
    /// warnings via `rotate_every_days`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_rotated_at: Option<String>,

    /// Recommended rotation cadence in days. `doctor` warns when
    /// `now > last_rotated_at + rotate_every_days - 7d`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotate_every_days: Option<u32>,

    /// How the secret is rotated. Defaults to [`RotationMethod::Manual`]
    /// at consumption time when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotation_method: Option<RotationMethod>,

    /// Advisory list of API scopes required for this token to function.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_scopes: Vec<String>,

    /// Reference into the `devboy-secret-patterns` catalogue. When
    /// set, the catalogue supplies sensible defaults for `format_regex`,
    /// `retrieval_url`, `rotation_method`, and `default_expiry_days`
    /// — see ADR-020 §3 and epic phase P2.4 for the wiring.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pattern_id: Option<String>,

    /// CI/headless override for the env-store source: when present,
    /// the env-store reads `<env_var>` verbatim instead of the
    /// convention-based name. See ADR-021 §8.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env_var: Option<String>,

    /// Per-secret upper bound on the source router's adaptive cache
    /// TTL. Cannot raise it above the source default — see ADR-021 §7.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_ttl_seconds_max: Option<u64>,

    /// Approve-on-use policy (P25). Defaults to `Never` at the
    /// consumer side when absent — see [`ApproveOnUse`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approve_on_use: Option<ApproveOnUse>,
}

/// In-memory representation of the global index.
///
/// Backed by a [`BTreeMap`] so iteration order is deterministic
/// (paths sort lexicographically), which keeps `secrets list` output
/// stable across runs.
#[derive(Debug, Clone, Default)]
pub struct GlobalIndex {
    entries: BTreeMap<SecretPath, IndexEntry>,
}

/// Internal serde shape for the on-disk file.
///
/// We can't deserialize directly into `BTreeMap<SecretPath, IndexEntry>`
/// because TOML key types are limited to strings: a custom flow is
/// needed to convert `String` → `SecretPath` with the validator.
#[derive(Debug, Default, Deserialize, Serialize)]
struct RawIndex {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    secret: BTreeMap<String, IndexEntry>,
}

impl GlobalIndex {
    /// Build an empty index. Useful for tests and as the default when
    /// the on-disk file is absent.
    pub fn new() -> Self {
        Self::default()
    }

    /// Resolve the canonical on-disk path
    /// (`<config-dir>/devboy-tools/secrets/index.toml`).
    ///
    /// Errors only if `dirs::config_dir()` returns `None`.
    pub fn default_path() -> Result<PathBuf, IndexError> {
        let dir = dirs::config_dir().ok_or(IndexError::NoConfigDir)?;
        Ok(dir
            .join("devboy-tools")
            .join(SECRETS_SUBDIR)
            .join(INDEX_FILENAME))
    }

    /// Load the index from the canonical default path.
    ///
    /// Returns an empty index if the file does not exist (the global
    /// index is opt-in).
    pub fn load() -> Result<Self, IndexError> {
        let path = Self::default_path()?;
        Self::load_from(&path)
    }

    /// Load the index from a specific path. Returns an empty index if
    /// the file does not exist.
    pub fn load_from(path: &Path) -> Result<Self, IndexError> {
        if !path.exists() {
            debug!(path = ?path, "global secrets index not present, using empty");
            return Ok(Self::new());
        }
        let body = fs::read_to_string(path).map_err(|e| IndexError::Read {
            path: path.to_path_buf(),
            source: e,
        })?;
        Self::from_str_with_path(&body, path)
    }

    /// Parse the index from a TOML string with no associated path
    /// (used in tests and when the caller already has the bytes in
    /// memory).
    pub fn from_toml_str(body: &str) -> Result<Self, IndexError> {
        Self::from_str_with_path(body, Path::new("<inline>"))
    }

    fn from_str_with_path(body: &str, path: &Path) -> Result<Self, IndexError> {
        let raw: RawIndex = toml::from_str(body).map_err(|e| IndexError::Parse {
            path: path.to_path_buf(),
            source: e,
        })?;

        let mut entries = BTreeMap::new();
        for (raw_path, entry) in raw.secret {
            let p = SecretPath::parse(&raw_path).map_err(|e| IndexError::Path { source: e })?;
            entries.insert(p, entry);
        }

        Ok(Self { entries })
    }

    /// Persist this index to `path`, creating parent directories
    /// as needed. Used by the liveness flow (P9.2 / P9.3) to
    /// write upstream-reported `expires_at` back into the global
    /// metadata.
    pub fn save_to(&self, path: &Path) -> Result<(), IndexError> {
        let body = self.to_toml_string().map_err(|e| IndexError::Serialize {
            path: path.to_path_buf(),
            source: e,
        })?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| IndexError::Read {
                path: parent.to_path_buf(),
                source: e,
            })?;
        }
        fs::write(path, body).map_err(|e| IndexError::Read {
            path: path.to_path_buf(),
            source: e,
        })
    }

    /// Persist to the default path. Returns the path written for
    /// the caller's logging convenience.
    pub fn save(&self) -> Result<PathBuf, IndexError> {
        let path = Self::default_path()?;
        self.save_to(&path)?;
        Ok(path)
    }

    /// Update `entry.expires_at` for `path`. Returns `Ok(true)`
    /// when an entry existed and the field changed; `Ok(false)`
    /// when no entry exists at that path. The caller persists
    /// via [`save`](Self::save) / [`save_to`](Self::save_to).
    ///
    /// Used by the liveness flow (P9.2): a probe that returns
    /// [`LivenessResult::expires_at`] hands the value here, and
    /// the next `doctor` run sees the freshened expiry.
    ///
    /// [`LivenessResult::expires_at`]: devboy_core::liveness::LivenessResult
    pub fn record_expiry(&mut self, path: &SecretPath, expires_at: &str) -> bool {
        match self.entries.get_mut(path) {
            Some(entry) => {
                let new = Some(expires_at.to_owned());
                if entry.expires_at != new {
                    entry.expires_at = new;
                    true
                } else {
                    false
                }
            }
            None => false,
        }
    }

    /// Update `entry.last_rotated_at` for `path`. Same shape as
    /// [`record_expiry`](Self::record_expiry); used by the rotation
    /// flow (P13.1) after a successful rotation.
    pub fn record_rotation(&mut self, path: &SecretPath, last_rotated_at: &str) -> bool {
        match self.entries.get_mut(path) {
            Some(entry) => {
                let new = Some(last_rotated_at.to_owned());
                if entry.last_rotated_at != new {
                    entry.last_rotated_at = new;
                    true
                } else {
                    false
                }
            }
            None => false,
        }
    }

    /// Serialize back to TOML. Round-trips bit-for-bit with what
    /// `from_toml_str` would parse (modulo whitespace and key order —
    /// the latter is stable thanks to [`BTreeMap`]).
    pub fn to_toml_string(&self) -> Result<String, toml::ser::Error> {
        let raw = RawIndex {
            secret: self
                .entries
                .iter()
                .map(|(k, v)| (k.as_str().to_owned(), v.clone()))
                .collect(),
        };
        toml::to_string_pretty(&raw)
    }

    /// Look up a single entry by path.
    pub fn get(&self, path: &SecretPath) -> Option<&IndexEntry> {
        self.entries.get(path)
    }

    /// Insert or replace an entry. Returns the previous entry if any.
    pub fn insert(&mut self, path: SecretPath, entry: IndexEntry) -> Option<IndexEntry> {
        self.entries.insert(path, entry)
    }

    /// Remove an entry. Returns the removed entry if present.
    pub fn remove(&mut self, path: &SecretPath) -> Option<IndexEntry> {
        self.entries.remove(path)
    }

    /// Total number of entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// `true` when the index has no entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Iterate over `(path, entry)` pairs in sorted order.
    pub fn iter(&self) -> impl Iterator<Item = (&SecretPath, &IndexEntry)> {
        self.entries.iter()
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// A complete entry with every field populated, used to exercise
    /// the round-trip and field-by-field deserialization paths.
    fn fixture_full_entry_toml() -> &'static str {
        r#"
[secret."team/gitlab/token-deploy"]
description       = "Deploy token for the team GitLab; used by CI mirrors and devboy plugins"
retrieval_url     = "https://gitlab.example.internal/-/profile/personal_access_tokens"
format_regex      = "^glpat-[A-Za-z0-9_-]{20,}$"
default_gate      = "auto"
expires_at        = "2026-08-01"
last_rotated_at   = "2026-05-02"
rotate_every_days = 90
rotation_method   = "manual"
required_scopes   = ["api", "read_repository"]
pattern_id        = "gitlab-pat"
env_var           = "GITLAB_TOKEN_DEPLOY"
cache_ttl_seconds_max = 60
"#
    }

    #[test]
    fn empty_string_yields_empty_index() {
        let idx = GlobalIndex::from_toml_str("").unwrap();
        assert!(idx.is_empty());
        assert_eq!(idx.len(), 0);
    }

    #[test]
    fn parses_full_entry() {
        let idx = GlobalIndex::from_toml_str(fixture_full_entry_toml()).unwrap();
        assert_eq!(idx.len(), 1);
        let path: SecretPath = "team/gitlab/token-deploy".parse().unwrap();
        let entry = idx.get(&path).expect("entry must be present");

        assert_eq!(
            entry.description.as_deref(),
            Some("Deploy token for the team GitLab; used by CI mirrors and devboy plugins")
        );
        assert_eq!(
            entry.retrieval_url.as_deref(),
            Some("https://gitlab.example.internal/-/profile/personal_access_tokens")
        );
        assert_eq!(
            entry.format_regex.as_deref(),
            Some("^glpat-[A-Za-z0-9_-]{20,}$")
        );
        assert_eq!(entry.default_gate, Some(Gate::Auto));
        assert_eq!(entry.expires_at.as_deref(), Some("2026-08-01"));
        assert_eq!(entry.last_rotated_at.as_deref(), Some("2026-05-02"));
        assert_eq!(entry.rotate_every_days, Some(90));
        assert_eq!(entry.rotation_method, Some(RotationMethod::Manual));
        assert_eq!(entry.required_scopes, vec!["api", "read_repository"]);
        assert_eq!(entry.pattern_id.as_deref(), Some("gitlab-pat"));
        assert_eq!(entry.env_var.as_deref(), Some("GITLAB_TOKEN_DEPLOY"));
        assert_eq!(entry.cache_ttl_seconds_max, Some(60));
    }

    #[test]
    fn parses_minimal_entry_with_defaults() {
        let idx = GlobalIndex::from_toml_str(
            r#"
[secret."personal/github/pat"]
description = "Personal GitHub PAT"
"#,
        )
        .unwrap();
        let p: SecretPath = "personal/github/pat".parse().unwrap();
        let e = idx.get(&p).unwrap();
        assert_eq!(e.description.as_deref(), Some("Personal GitHub PAT"));
        assert!(e.retrieval_url.is_none());
        assert!(e.format_regex.is_none());
        assert!(e.default_gate.is_none());
        assert!(e.required_scopes.is_empty());
    }

    #[test]
    fn parses_multiple_entries_sorted() {
        let idx = GlobalIndex::from_toml_str(
            r#"
[secret."team/openai/api-key"]
description = "Team OpenAI"

[secret."personal/github/pat"]
description = "Personal GitHub"

[secret."client-acme/jira/api-key"]
description = "Acme Jira"
"#,
        )
        .unwrap();
        assert_eq!(idx.len(), 3);

        let paths: Vec<&str> = idx.iter().map(|(p, _)| p.as_str()).collect();
        // BTreeMap iteration is sorted lexicographically.
        assert_eq!(
            paths,
            vec![
                "client-acme/jira/api-key",
                "personal/github/pat",
                "team/openai/api-key",
            ]
        );
    }

    #[test]
    fn rejects_invalid_path_in_key() {
        // Key violates ADR-020 §2 (only two segments).
        let err = GlobalIndex::from_toml_str(
            r#"
[secret."gitlab/token"]
description = "wrong"
"#,
        )
        .unwrap_err();
        match err {
            IndexError::Path { source } => {
                assert!(matches!(source, PathError::TooFewSegments { found: 2, .. }));
            }
            other => panic!("expected Path error, got {other:?}"),
        }
    }

    #[test]
    fn rejects_reserved_prefix_in_key() {
        let err = GlobalIndex::from_toml_str(
            r#"
[secret."__sources/vault/deploy"]
description = "internal"
"#,
        )
        .unwrap_err();
        assert!(matches!(
            err,
            IndexError::Path {
                source: PathError::ReservedPrefix { .. }
            }
        ));
    }

    #[test]
    fn rejects_unknown_field() {
        // `deny_unknown_fields` guards against typos like `retrieval_hint`
        // (the ADR-020 v1 spelling that was renamed to `retrieval_url`).
        let err = GlobalIndex::from_toml_str(
            r#"
[secret."team/gitlab/token-deploy"]
retrieval_hint = "wrong field name"
"#,
        )
        .unwrap_err();
        assert!(matches!(err, IndexError::Parse { .. }));
    }

    #[test]
    fn parses_each_gate_value() {
        for (literal, expected) in [
            ("auto", Gate::Auto),
            ("confirm", Gate::Confirm),
            ("touchid", Gate::Touchid),
        ] {
            let toml =
                format!("[secret.\"team/gitlab/token-deploy\"]\ndefault_gate = \"{literal}\"\n");
            let idx = GlobalIndex::from_toml_str(&toml).unwrap();
            let p: SecretPath = "team/gitlab/token-deploy".parse().unwrap();
            assert_eq!(idx.get(&p).unwrap().default_gate, Some(expected));
        }
    }

    #[test]
    fn parses_each_rotation_method_value() {
        for (literal, expected) in [
            ("manual", RotationMethod::Manual),
            ("provider-ui", RotationMethod::ProviderUi),
            ("provider-api", RotationMethod::ProviderApi),
        ] {
            let toml =
                format!("[secret.\"team/gitlab/token-deploy\"]\nrotation_method = \"{literal}\"\n");
            let idx = GlobalIndex::from_toml_str(&toml).unwrap();
            let p: SecretPath = "team/gitlab/token-deploy".parse().unwrap();
            assert_eq!(idx.get(&p).unwrap().rotation_method, Some(expected));
        }
    }

    #[test]
    fn round_trip_full_entry() {
        let idx = GlobalIndex::from_toml_str(fixture_full_entry_toml()).unwrap();
        let serialized = idx.to_toml_string().unwrap();
        let reparsed = GlobalIndex::from_toml_str(&serialized).unwrap();
        assert_eq!(idx.len(), reparsed.len());
        let p: SecretPath = "team/gitlab/token-deploy".parse().unwrap();
        assert_eq!(idx.get(&p), reparsed.get(&p));
    }

    #[test]
    fn approve_on_use_round_trips_through_toml() {
        // Default (None) round-trips as missing field.
        let mut idx = GlobalIndex::new();
        let p: SecretPath = "team/gitlab/token-deploy".parse().unwrap();
        idx.insert(p.clone(), IndexEntry::default());
        let body = idx.to_toml_string().unwrap();
        assert!(
            !body.contains("approve_on_use"),
            "missing approve_on_use should not be serialised: {body}"
        );

        // Per-call policy round-trips.
        let entry = IndexEntry {
            approve_on_use: Some(ApproveOnUse::PerCall),
            ..IndexEntry::default()
        };
        let mut idx = GlobalIndex::new();
        idx.insert(p.clone(), entry.clone());
        let body = idx.to_toml_string().unwrap();
        assert!(
            body.contains("approve_on_use = \"per-call\""),
            "expected kebab-case `per-call` in: {body}"
        );
        let reparsed = GlobalIndex::from_toml_str(&body).unwrap();
        assert_eq!(reparsed.get(&p), Some(&entry));

        // Session policy round-trips.
        let entry = IndexEntry {
            approve_on_use: Some(ApproveOnUse::Session),
            ..IndexEntry::default()
        };
        let mut idx = GlobalIndex::new();
        idx.insert(p.clone(), entry.clone());
        let body = idx.to_toml_string().unwrap();
        assert!(body.contains("approve_on_use = \"session\""));
        let reparsed = GlobalIndex::from_toml_str(&body).unwrap();
        assert_eq!(reparsed.get(&p), Some(&entry));
    }

    #[test]
    fn insert_remove_get() {
        let mut idx = GlobalIndex::new();
        let p: SecretPath = "team/gitlab/token-deploy".parse().unwrap();
        let entry = IndexEntry {
            description: Some("test".to_owned()),
            ..IndexEntry::default()
        };
        assert!(idx.insert(p.clone(), entry.clone()).is_none());
        assert_eq!(idx.get(&p), Some(&entry));
        assert_eq!(idx.remove(&p), Some(entry));
        assert!(idx.get(&p).is_none());
    }

    #[test]
    fn load_from_returns_empty_when_file_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("never-existed.toml");
        let idx = GlobalIndex::load_from(&path).unwrap();
        assert!(idx.is_empty());
    }

    #[test]
    fn load_from_real_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("index.toml");
        std::fs::write(&path, fixture_full_entry_toml()).unwrap();
        let idx = GlobalIndex::load_from(&path).unwrap();
        assert_eq!(idx.len(), 1);
    }

    #[test]
    fn load_from_io_error_surfaces_path() {
        // Pointing at a *directory* triggers a read error.
        let dir = tempfile::tempdir().unwrap();
        let err = GlobalIndex::load_from(dir.path()).unwrap_err();
        match err {
            IndexError::Read { path, .. } => assert_eq!(path, dir.path()),
            other => panic!("expected Read, got {other:?}"),
        }
    }

    #[test]
    fn default_path_includes_secrets_subdir_and_index_filename() {
        let p = GlobalIndex::default_path().unwrap();
        let s = p.to_string_lossy();
        assert!(s.ends_with("/secrets/index.toml") || s.ends_with("\\secrets\\index.toml"));
        assert!(s.contains("devboy-tools"));
    }

    #[test]
    fn secret_path_serde_roundtrip_via_index() {
        // Indirectly exercises Serialize/Deserialize on SecretPath.
        let idx = GlobalIndex::from_toml_str(
            r#"
[secret."team/gitlab/token-deploy"]
description = "x"
"#,
        )
        .unwrap();
        let s = idx.to_toml_string().unwrap();
        assert!(s.contains("team/gitlab/token-deploy"));
    }
}
