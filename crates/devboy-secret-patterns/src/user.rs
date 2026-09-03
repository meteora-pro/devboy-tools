//! User-supplied pattern extension per [ADR-023] §3.6.
//!
//! Users can extend the catalogue without rebuilding `devboy-tools`
//! by dropping TOML files into `~/.devboy/secrets/patterns.d/*.toml`.
//! Each file declares one or more patterns:
//!
//! ```toml
//! [[pattern]]
//! id              = "internal-mfa-token"
//! display_name    = "Internal MFA Service Token"
//! format_regex    = "^mfa_[A-Z0-9]{40}$"
//! severity        = "high"
//! provider_id     = "internal"
//! retrieval_url_template = "https://mfa.example.internal/tokens"
//! default_expiry_days = 180
//! scopes_hint     = ["read", "write"]
//! ```
//!
//! Loading walks the directory once at startup, deserialises each
//! file, compiles every regex up-front (so a malformed pattern is a
//! load-time error, not a hot-path surprise), and merges with the
//! built-in catalogue from [`crate::builtin`].
//!
//! # Shadowing
//!
//! A user pattern with the same `id` as a built-in **shadows** the
//! built-in: the user version wins from [`Catalogue::find`] and is
//! the only one returned by [`Catalogue::iter`]. This is by design
//! — a user might want to override the metadata or the regex for a
//! provider whose built-in entry is wrong for their environment. To
//! keep the override visible, a [`LoadWarning`] of kind
//! [`LoadWarningKind::ShadowsBuiltin`] is recorded.
//!
//! [ADR-023]: https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-023-secret-store-ux-layer.md

use std::borrow::Cow;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::warn;

use crate::builtin;
use crate::{PatternMetadata, SecretPattern, Severity};

/// Subdirectory under the user's `~/.devboy/secrets/` directory that
/// holds extension TOML files.
pub const USER_PATTERNS_SUBDIR: &str = "patterns.d";

// =============================================================================
// On-disk schema
// =============================================================================

/// Top-level shape of a single TOML file under `patterns.d/`.
#[derive(Debug, Default, Clone, Deserialize, Serialize)]
pub struct UserPatternFile {
    /// Zero or more patterns declared in this file.
    #[serde(default, rename = "pattern")]
    pub patterns: Vec<UserPatternEntry>,
}

/// On-disk representation of a single user pattern. Mirrors the
/// shape from ADR-023 §3.6.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UserPatternEntry {
    /// Stable identifier (lowercase kebab-case, see
    /// `is_kebab_id`).
    pub id: String,
    /// Human-readable name shown in the metadata card and scan
    /// reports.
    pub display_name: String,
    /// Regex source. Compiled at load time; a malformed regex is a
    /// load error.
    pub format_regex: String,
    /// Severity. Lowercase tokens (`"low"`, `"medium"`, `"high"`)
    /// per the [`Severity`] serde shape.
    pub severity: Severity,

    /// Optional provider id (`"github"`, `"internal"`, ...). When
    /// either of `provider_id` or `retrieval_url_template` is set,
    /// the loader produces a [`PatternMetadata`] for the resulting
    /// pattern. The two fields are independent — a pattern can have
    /// a retrieval URL without naming a provider, or vice versa.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_id: Option<String>,
    /// Optional URL template the user opens to obtain a fresh
    /// value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retrieval_url_template: Option<String>,
    /// Optional default rotation cadence in days.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_expiry_days: Option<u32>,
    /// Optional scope hints. Surfaced in the metadata card; does
    /// not validate.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scopes_hint: Vec<String>,
}

// =============================================================================
// Compiled in-memory pattern
// =============================================================================

/// User pattern after deserialisation + regex compilation. Holds owned
/// strings (unlike [`crate::builtin::Builtin`], which is `'static`).
#[derive(Debug)]
pub struct UserPattern {
    id: String,
    display_name: String,
    severity: Severity,
    regex: Regex,
    /// Scanning form of `regex`, derived at load time. `None` when the
    /// pattern is unfit to scan free text — see [`crate::scanning`].
    /// Derived here rather than lazily because the whole file is
    /// compiled up-front anyway, and a user pattern that will never
    /// scan is worth knowing about at load.
    scan: Option<Regex>,
    metadata: Option<PatternMetadata>,
}

impl UserPattern {
    /// Source path of the file the pattern was loaded from. Currently
    /// not stored; kept on the entry as a future hook for surfacing
    /// "this user pattern was defined at `<path>`" in `doctor` output.
    pub fn id(&self) -> &str {
        &self.id
    }
}

impl SecretPattern for UserPattern {
    fn id(&self) -> &str {
        &self.id
    }
    fn display_name(&self) -> &str {
        &self.display_name
    }
    fn format_regex(&self) -> &Regex {
        &self.regex
    }
    fn scan_regex(&self) -> Option<&Regex> {
        self.scan.as_ref()
    }
    fn severity(&self) -> Severity {
        self.severity
    }
    fn metadata(&self) -> Option<&PatternMetadata> {
        self.metadata.as_ref()
    }
    // rotation() and liveness() use the trait defaults (None).
}

// =============================================================================
// Errors and warnings
// =============================================================================

/// Hard errors during user-pattern loading.
#[derive(Debug, Error)]
pub enum LoadError {
    /// I/O error reading the patterns directory or one of its files.
    #[error("failed to read user patterns at {path}: {source}")]
    Read {
        /// Path that failed to read.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },

    /// TOML deserialisation error in one of the files.
    #[error("failed to parse user patterns file at {path}: {source}")]
    Parse {
        /// File that failed to parse.
        path: PathBuf,
        /// Underlying TOML deserialisation error.
        #[source]
        source: Box<toml::de::Error>,
    },

    /// A pattern's `format_regex` is malformed.
    #[error("invalid regex for user pattern '{id}' in {path}: {source}")]
    BadRegex {
        /// Source file.
        path: PathBuf,
        /// Pattern id.
        id: String,
        /// Underlying regex compile error.
        #[source]
        source: regex::Error,
    },

    /// A pattern's `id` violates the lowercase-kebab convention or is
    /// empty.
    #[error("user pattern id '{id}' in {path} must be lowercase kebab-case (`[a-z][a-z0-9-]*`)")]
    BadId {
        /// Source file.
        path: PathBuf,
        /// Offending id.
        id: String,
    },

    /// Two user patterns share the same `id` (across files or within
    /// one file). The user has to disambiguate before the catalogue
    /// can be loaded.
    #[error("user pattern id '{id}' is declared twice (first in {first}, then in {second})")]
    DuplicateUserId {
        /// The conflicting id.
        id: String,
        /// File that declared it first.
        first: PathBuf,
        /// File that declared it again.
        second: PathBuf,
    },
}

/// Non-fatal warning surfaced through `doctor` after a successful
/// load.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadWarning {
    /// What the warning is about.
    pub kind: LoadWarningKind,
    /// Identifier or filename the warning concerns.
    pub subject: String,
}

impl std::fmt::Display for LoadWarning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.kind {
            LoadWarningKind::ShadowsBuiltin => write!(
                f,
                "`{}` shadows a built-in pattern of the same id; yours wins",
                self.subject
            ),
            LoadWarningKind::SkippedNonToml => {
                write!(f, "skipped {} — not a .toml file", self.subject)
            }
        }
    }
}

/// Categories of advisory warnings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadWarningKind {
    /// A user pattern uses the same `id` as a built-in pattern. The
    /// user version wins; this is recorded so `doctor` can hint
    /// "you're shadowing the built-in `<id>`".
    ShadowsBuiltin,
    /// A non-`.toml` file was found in `patterns.d/` and skipped.
    /// Useful for catching stray editor backups (`*.toml.bak`,
    /// `*.swp`).
    SkippedNonToml,
}

// =============================================================================
// Catalogue (built-in + user)
// =============================================================================

/// Combined view of the built-in catalogue plus any user patterns
/// loaded from `~/.devboy/secrets/patterns.d/*.toml`.
#[derive(Debug, Default)]
pub struct Catalogue {
    user_patterns: Vec<UserPattern>,
    /// Set of built-in `id`s that the user shadowed.
    shadowed: HashSet<String>,
    warnings: Vec<LoadWarning>,
}

impl Catalogue {
    /// Catalogue with no user patterns — equivalent to the built-in
    /// list alone.
    pub fn builtins_only() -> Self {
        Self::default()
    }

    /// Load user patterns from a directory and merge with built-ins.
    ///
    /// If `dir` does not exist, the catalogue contains only built-ins
    /// (no error, no warning — the directory is opt-in).
    pub fn load(dir: &Path) -> Result<Self, LoadError> {
        let mut cat = Self::default();
        if !dir.exists() {
            return Ok(cat);
        }

        let read = fs::read_dir(dir).map_err(|e| LoadError::Read {
            path: dir.to_path_buf(),
            source: e,
        })?;

        // Stable file order so error messages and warnings are
        // reproducible across runs (filesystems do not guarantee
        // ordering).
        let mut paths: Vec<PathBuf> = read.filter_map(|res| res.ok().map(|e| e.path())).collect();
        paths.sort();

        // Track first-declaration source for duplicate detection.
        let mut id_origin: std::collections::HashMap<String, PathBuf> =
            std::collections::HashMap::new();

        for path in paths {
            if path.extension().and_then(|s| s.to_str()) != Some("toml") {
                cat.warnings.push(LoadWarning {
                    kind: LoadWarningKind::SkippedNonToml,
                    subject: path.display().to_string(),
                });
                continue;
            }

            let body = fs::read_to_string(&path).map_err(|e| LoadError::Read {
                path: path.clone(),
                source: e,
            })?;
            let parsed: UserPatternFile = toml::from_str(&body).map_err(|e| LoadError::Parse {
                path: path.clone(),
                source: Box::new(e),
            })?;

            for entry in parsed.patterns {
                if !is_kebab_id(&entry.id) {
                    return Err(LoadError::BadId {
                        path: path.clone(),
                        id: entry.id,
                    });
                }

                if let Some(prev) = id_origin.get(&entry.id) {
                    return Err(LoadError::DuplicateUserId {
                        id: entry.id,
                        first: prev.clone(),
                        second: path.clone(),
                    });
                }

                // R5 (PR #265 review) — clamp the compiled DFA
                // to 64 KiB. The default regex `size_limit` is
                // 10 MiB; a malformed user pattern hitting that
                // bound surfaces a generic "compiled regex
                // exceeds size limit" error with the path long
                // gone from the call chain. 64 KiB is far above
                // anything legitimate (the bundled catalogue's
                // largest regex is < 200 bytes) and the failure
                // now points at the offending toml + entry id.
                let regex = RegexBuilder::new(&entry.format_regex)
                    .size_limit(64 * 1024)
                    .build()
                    .map_err(|e| LoadError::BadRegex {
                        path: path.clone(),
                        id: entry.id.clone(),
                        source: e,
                    })?;

                let metadata = if entry.provider_id.is_some()
                    || entry.retrieval_url_template.is_some()
                    || entry.default_expiry_days.is_some()
                    || !entry.scopes_hint.is_empty()
                {
                    Some(PatternMetadata {
                        provider_id: Cow::Owned(entry.provider_id.unwrap_or_default()),
                        retrieval_url_template: Cow::Owned(
                            entry.retrieval_url_template.unwrap_or_default(),
                        ),
                        default_expiry_days: entry.default_expiry_days,
                        scopes_hint: entry.scopes_hint.into_iter().map(Cow::Owned).collect(),
                    })
                } else {
                    None
                };

                if builtin::find(&entry.id).is_some() {
                    cat.shadowed.insert(entry.id.clone());
                    cat.warnings.push(LoadWarning {
                        kind: LoadWarningKind::ShadowsBuiltin,
                        subject: entry.id.clone(),
                    });
                    warn!(
                        id = %entry.id,
                        path = %path.display(),
                        "user pattern shadows built-in entry"
                    );
                }

                // Derived from the same source the regex was built
                // from, so a user pattern gets embedded-match
                // redaction on exactly the same terms as a built-in.
                let scan = crate::scanning::scanning_regex(&entry.format_regex);

                id_origin.insert(entry.id.clone(), path.clone());
                cat.user_patterns.push(UserPattern {
                    id: entry.id,
                    display_name: entry.display_name,
                    severity: entry.severity,
                    regex,
                    scan,
                    metadata,
                });
            }
        }

        Ok(cat)
    }

    /// Walk every visible pattern (user + non-shadowed built-ins).
    /// User patterns come first so iteration order is stable when
    /// a user override exists.
    pub fn iter(&self) -> Vec<&dyn SecretPattern> {
        let mut out: Vec<&dyn SecretPattern> = self
            .user_patterns
            .iter()
            .map(|p| p as &dyn SecretPattern)
            .collect();
        for b in builtin::builtins() {
            if !self.shadowed.contains(b.id()) {
                out.push(b);
            }
        }
        out
    }

    /// Look up a pattern by id, preferring a user override over the
    /// built-in.
    pub fn find(&self, id: &str) -> Option<&dyn SecretPattern> {
        if let Some(p) = self.user_patterns.iter().find(|p| p.id() == id) {
            return Some(p as &dyn SecretPattern);
        }
        builtin::find(id)
    }

    /// Non-fatal warnings produced during loading (shadowed built-in
    /// ids, skipped non-toml files, …). `doctor` consumes these.
    pub fn warnings(&self) -> &[LoadWarning] {
        &self.warnings
    }

    /// `true` when the catalogue holds any user-supplied pattern.
    pub fn has_user_patterns(&self) -> bool {
        !self.user_patterns.is_empty()
    }
}

// =============================================================================
// Internal helpers
// =============================================================================

/// `[a-z][a-z0-9-]*` — same shape as the
/// [`devboy_storage::SecretPath`] segment validator. Pattern ids and
/// path segments share this convention so a pattern id can be used
/// as a path provider segment without re-mapping.
fn is_kebab_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    if bytes.is_empty() {
        return false;
    }
    if !bytes[0].is_ascii_lowercase() {
        return false;
    }
    bytes
        .iter()
        .skip(1)
        .all(|&b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn write_toml(dir: &Path, name: &str, body: &str) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, body).expect("write fixture");
        p
    }

    // -- Loading happy paths --------------------------------------------------

    #[test]
    fn missing_directory_yields_empty_catalogue() {
        let dir = tempfile::tempdir().unwrap();
        let cat = Catalogue::load(&dir.path().join("nonexistent")).unwrap();
        assert!(!cat.has_user_patterns());
        assert!(cat.warnings.is_empty());
    }

    #[test]
    fn empty_directory_yields_empty_catalogue() {
        let dir = tempfile::tempdir().unwrap();
        let cat = Catalogue::load(dir.path()).unwrap();
        assert!(!cat.has_user_patterns());
        assert!(cat.warnings.is_empty());
    }

    #[test]
    fn loads_single_user_pattern() {
        let dir = tempfile::tempdir().unwrap();
        write_toml(
            dir.path(),
            "internal.toml",
            r#"
[[pattern]]
id           = "internal-mfa-token"
display_name = "Internal MFA Token"
format_regex = "^mfa_[A-Z0-9]{40}$"
severity     = "high"
provider_id  = "internal"
retrieval_url_template = "https://mfa.example.internal/tokens"
default_expiry_days = 180
scopes_hint  = ["read", "write"]
"#,
        );

        let cat = Catalogue::load(dir.path()).unwrap();
        assert!(cat.has_user_patterns());
        let p = cat.find("internal-mfa-token").expect("found");
        assert_eq!(p.id(), "internal-mfa-token");
        assert_eq!(p.display_name(), "Internal MFA Token");
        assert_eq!(p.severity(), Severity::High);
        assert!(
            p.format_regex()
                .is_match("mfa_ABCDEFGHIJ0123456789ABCDEFGHIJ0123456789")
        );
        let m = p.metadata().expect("metadata");
        assert_eq!(m.provider_id.as_ref(), "internal");
        assert_eq!(
            m.retrieval_url_template.as_ref(),
            "https://mfa.example.internal/tokens"
        );
        assert_eq!(m.default_expiry_days, Some(180));
        assert_eq!(m.scopes_hint.len(), 2);
    }

    #[test]
    fn user_pattern_without_metadata_fields_has_no_metadata() {
        let dir = tempfile::tempdir().unwrap();
        write_toml(
            dir.path(),
            "minimal.toml",
            r#"
[[pattern]]
id           = "minimal-x"
display_name = "Minimal X"
format_regex = "^x_[a-z]{8}$"
severity     = "low"
"#,
        );

        let cat = Catalogue::load(dir.path()).unwrap();
        let p = cat.find("minimal-x").unwrap();
        assert!(p.metadata().is_none());
    }

    #[test]
    fn loads_multiple_files_in_sorted_order() {
        let dir = tempfile::tempdir().unwrap();
        write_toml(
            dir.path(),
            "b.toml",
            r#"
[[pattern]]
id           = "second"
display_name = "Second"
format_regex = "^.+$"
severity     = "low"
"#,
        );
        write_toml(
            dir.path(),
            "a.toml",
            r#"
[[pattern]]
id           = "first"
display_name = "First"
format_regex = "^.+$"
severity     = "low"
"#,
        );

        let cat = Catalogue::load(dir.path()).unwrap();
        // a.toml sorts before b.toml so `first` was inserted first.
        assert_eq!(cat.user_patterns[0].id(), "first");
        assert_eq!(cat.user_patterns[1].id(), "second");
    }

    #[test]
    fn skips_non_toml_files_with_warning() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("readme.md"), "not toml").unwrap();
        write_toml(
            dir.path(),
            "real.toml",
            r#"
[[pattern]]
id           = "real-x"
display_name = "Real"
format_regex = "^.+$"
severity     = "low"
"#,
        );

        let cat = Catalogue::load(dir.path()).unwrap();
        assert_eq!(cat.user_patterns.len(), 1);
        assert!(
            cat.warnings
                .iter()
                .any(|w| matches!(w.kind, LoadWarningKind::SkippedNonToml))
        );
    }

    // -- Shadowing -----------------------------------------------------------

    #[test]
    fn user_pattern_shadows_builtin_with_warning() {
        let dir = tempfile::tempdir().unwrap();
        write_toml(
            dir.path(),
            "shadow.toml",
            r#"
[[pattern]]
id           = "github-pat"
display_name = "Custom GitHub PAT"
format_regex = "^my-custom-gh-.+$"
severity     = "high"
"#,
        );

        let cat = Catalogue::load(dir.path()).unwrap();
        let p = cat.find("github-pat").expect("found via user");
        assert_eq!(p.display_name(), "Custom GitHub PAT");
        assert!(p.format_regex().is_match("my-custom-gh-anything"));
        assert!(!p.format_regex().is_match("ghp_someValidLookingTokenString"));

        // The shadowed built-in is no longer in the iter view.
        let visible_ids: Vec<&str> = cat.iter().iter().map(|p| p.id()).collect();
        // Only ONE entry with id "github-pat" — the user version.
        let count = visible_ids.iter().filter(|id| **id == "github-pat").count();
        assert_eq!(count, 1);
        assert!(cat.warnings.iter().any(
            |w| matches!(w.kind, LoadWarningKind::ShadowsBuiltin) && w.subject == "github-pat"
        ));
    }

    #[test]
    fn non_shadowing_user_patterns_coexist_with_builtins() {
        let dir = tempfile::tempdir().unwrap();
        write_toml(
            dir.path(),
            "x.toml",
            r#"
[[pattern]]
id           = "internal-x"
display_name = "Internal X"
format_regex = "^x_.+$"
severity     = "low"
"#,
        );

        let cat = Catalogue::load(dir.path()).unwrap();
        let visible: Vec<&dyn SecretPattern> = cat.iter();
        // Built-ins (31) + 1 user.
        assert_eq!(visible.len(), 32);
        // No shadow warnings for non-shadowing user patterns.
        assert!(
            !cat.warnings
                .iter()
                .any(|w| matches!(w.kind, LoadWarningKind::ShadowsBuiltin))
        );
    }

    // -- Hard errors ---------------------------------------------------------

    #[test]
    fn rejects_bad_regex() {
        let dir = tempfile::tempdir().unwrap();
        write_toml(
            dir.path(),
            "bad.toml",
            r#"
[[pattern]]
id           = "bad-regex"
display_name = "Bad Regex"
format_regex = "^[unclosed$"
severity     = "low"
"#,
        );

        let err = Catalogue::load(dir.path()).unwrap_err();
        match err {
            LoadError::BadRegex { id, .. } => assert_eq!(id, "bad-regex"),
            other => panic!("expected BadRegex, got {other:?}"),
        }
    }

    #[test]
    fn rejects_bad_id_uppercase() {
        let dir = tempfile::tempdir().unwrap();
        write_toml(
            dir.path(),
            "bad.toml",
            r#"
[[pattern]]
id           = "BadId"
display_name = "x"
format_regex = "^.+$"
severity     = "low"
"#,
        );

        let err = Catalogue::load(dir.path()).unwrap_err();
        assert!(matches!(err, LoadError::BadId { .. }));
    }

    #[test]
    fn rejects_bad_id_starts_with_digit() {
        let dir = tempfile::tempdir().unwrap();
        write_toml(
            dir.path(),
            "bad.toml",
            r#"
[[pattern]]
id           = "9bad"
display_name = "x"
format_regex = "^.+$"
severity     = "low"
"#,
        );
        assert!(matches!(
            Catalogue::load(dir.path()).unwrap_err(),
            LoadError::BadId { .. }
        ));
    }

    #[test]
    fn rejects_duplicate_user_id_across_files() {
        let dir = tempfile::tempdir().unwrap();
        write_toml(
            dir.path(),
            "a.toml",
            r#"
[[pattern]]
id           = "dup"
display_name = "First"
format_regex = "^a$"
severity     = "low"
"#,
        );
        write_toml(
            dir.path(),
            "b.toml",
            r#"
[[pattern]]
id           = "dup"
display_name = "Second"
format_regex = "^b$"
severity     = "low"
"#,
        );

        let err = Catalogue::load(dir.path()).unwrap_err();
        match err {
            LoadError::DuplicateUserId { id, .. } => assert_eq!(id, "dup"),
            other => panic!("expected DuplicateUserId, got {other:?}"),
        }
    }

    #[test]
    fn rejects_duplicate_user_id_within_file() {
        let dir = tempfile::tempdir().unwrap();
        write_toml(
            dir.path(),
            "a.toml",
            r#"
[[pattern]]
id           = "dup"
display_name = "First"
format_regex = "^a$"
severity     = "low"

[[pattern]]
id           = "dup"
display_name = "Second"
format_regex = "^b$"
severity     = "low"
"#,
        );

        let err = Catalogue::load(dir.path()).unwrap_err();
        assert!(matches!(err, LoadError::DuplicateUserId { .. }));
    }

    #[test]
    fn rejects_unknown_field() {
        let dir = tempfile::tempdir().unwrap();
        write_toml(
            dir.path(),
            "bad.toml",
            r#"
[[pattern]]
id           = "x"
display_name = "x"
format_regex = "^.+$"
severity     = "low"
unknown_field = "wrong"
"#,
        );
        assert!(matches!(
            Catalogue::load(dir.path()).unwrap_err(),
            LoadError::Parse { .. }
        ));
    }

    // -- Builtins-only catalogue --------------------------------------------

    #[test]
    fn builtins_only_returns_thirty_one_patterns() {
        let cat = Catalogue::builtins_only();
        let visible = cat.iter();
        assert_eq!(visible.len(), 31);
        assert!(cat.warnings.is_empty());
    }

    #[test]
    fn builtins_only_find_works() {
        let cat = Catalogue::builtins_only();
        let p = cat.find("github-pat").unwrap();
        assert_eq!(p.id(), "github-pat");
        assert!(cat.find("no-such-id").is_none());
    }

    // -- is_kebab_id helper --------------------------------------------------

    #[test]
    fn is_kebab_id_accepts_valid_ids() {
        assert!(is_kebab_id("github-pat"));
        assert!(is_kebab_id("a"));
        assert!(is_kebab_id("a1"));
        assert!(is_kebab_id("a-b-c"));
    }

    #[test]
    fn is_kebab_id_rejects_invalid_ids() {
        assert!(!is_kebab_id(""));
        assert!(!is_kebab_id("Github"));
        assert!(!is_kebab_id("9start"));
        assert!(!is_kebab_id("with_underscore"));
        assert!(!is_kebab_id("with.dot"));
        assert!(!is_kebab_id("-leading-dash"));
    }
}
