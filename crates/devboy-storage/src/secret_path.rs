//! Secret path validation per [ADR-020] §2.
//!
//! A *secret path* is the canonical name of a secret in the
//! `devboy-tools` namespace. It is a `/`-separated sequence of
//! lowercase kebab-case segments shaped as
//! `<scope>/<provider>/<purpose>` (minimum three segments).
//!
//! The validator is the entry gate for every layer above the
//! credential store — the manifest loader, the alias resolver, the
//! source router. Drift in the namespace silently degrades into
//! "every project invents its own pattern", so this module rejects
//! non-conforming paths as a *hard error*.
//!
//! # Examples
//!
//! ```
//! use devboy_storage::SecretPath;
//!
//! let p: SecretPath = "team/gitlab/token-deploy".parse().unwrap();
//! assert_eq!(p.scope(), "team");
//! assert_eq!(p.provider(), "gitlab");
//! assert_eq!(p.purpose(), "token-deploy");
//! assert!(!p.is_internal());
//!
//! assert!("gitlab.token".parse::<SecretPath>().is_err());
//! assert!("team/gitlab".parse::<SecretPath>().is_err());
//! assert!("__sources/vault/token".parse::<SecretPath>().is_err());
//! ```
//!
//! [ADR-020]: https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-020-secret-manifest-and-alias-resolution.md

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;

/// Reserved path prefixes that are rejected at user-facing entry points
/// per ADR-020 §2.
const RESERVED_INTERNAL_PREFIX: &str = "__";
const RESERVED_TEST_PREFIX: &str = "_test";

/// Failure modes when parsing a [`SecretPath`].
///
/// Each variant maps 1:1 onto a rule from ADR-020 §2 so callers can
/// surface a precise message to the user (or to `doctor`).
#[derive(Debug, Error, PartialEq, Eq, Clone)]
pub enum PathError {
    /// Fewer than three `/`-separated segments.
    ///
    /// Two-segment paths like `gitlab/token` are intentionally not
    /// allowed: they have no scope and silently encourage cross-context
    /// reuse of credentials that should be distinct.
    #[error(
        "secret path '{path}' has {found} segment(s); minimum 3 required (`<scope>/<provider>/<purpose>`)"
    )]
    TooFewSegments {
        /// The path the caller offered.
        path: String,
        /// Number of segments found.
        found: usize,
    },

    /// A segment was empty (e.g. `team//gitlab/token`).
    #[error("secret path '{path}' contains an empty segment at position {index}")]
    EmptySegment {
        /// The path the caller offered.
        path: String,
        /// Zero-based segment index that was empty.
        index: usize,
    },

    /// A segment did not match `[a-z][a-z0-9-]*`.
    #[error(
        "secret path '{path}' segment '{segment}' (position {index}) is not lowercase kebab-case (`[a-z][a-z0-9-]*`)"
    )]
    BadSegment {
        /// The path the caller offered.
        path: String,
        /// The offending segment.
        segment: String,
        /// Zero-based index of the bad segment.
        index: usize,
    },

    /// The first segment is one of the reserved framework prefixes
    /// (`__*` or `_test`) and the path was offered through a
    /// user-facing entry point.
    #[error(
        "secret path '{path}' uses reserved prefix '{prefix}'; that namespace is for framework-internal use"
    )]
    ReservedPrefix {
        /// The path the caller offered.
        path: String,
        /// The reserved scope segment.
        prefix: String,
    },
}

/// A validated secret path.
///
/// Construct one through [`SecretPath::parse`] (or `"...".parse()` via
/// [`FromStr`]). The wrapped string is guaranteed to satisfy every rule
/// in ADR-020 §2 at construction time, and the type is the only way
/// the rest of the framework consumes a path.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SecretPath(String);

impl SecretPath {
    /// Validate `s` and return an owned [`SecretPath`].
    ///
    /// Equivalent to `s.parse::<SecretPath>()`.
    pub fn parse(s: &str) -> Result<Self, PathError> {
        validate(s)?;
        Ok(Self(s.to_owned()))
    }

    /// Like [`SecretPath::parse`] but accepts internally-reserved
    /// prefixes (`__*`, `_test`).
    ///
    /// This entry point is **only** for the framework's own
    /// consumers — for example, the source-credential resolver in
    /// ADR-021 §5 reading paths under `__sources/...`. Production
    /// code that takes a path from the user must call
    /// [`SecretPath::parse`] instead.
    pub fn parse_internal(s: &str) -> Result<Self, PathError> {
        validate_with(s, /* allow_reserved = */ true)?;
        Ok(Self(s.to_owned()))
    }

    /// The full path, e.g. `"team/gitlab/token-deploy"`.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The first segment — the scope (e.g. `team`, `personal`,
    /// `client-acme`, `sandbox`, or one of the reserved prefixes when
    /// the path was constructed via [`SecretPath::parse_internal`]).
    pub fn scope(&self) -> &str {
        self.segment(0)
    }

    /// The second segment — usually a provider name
    /// (`gitlab`, `github`, `openai`, ...).
    pub fn provider(&self) -> &str {
        self.segment(1)
    }

    /// The third segment — usually a per-credential purpose
    /// (`token-deploy`, `pat`, `api-key`, ...).
    ///
    /// Paths longer than three segments preserve the rest as part of
    /// the purpose string returned here (joined with `/`).
    pub fn purpose(&self) -> &str {
        let mut byte_offset = 0;
        for (i, seg) in self.0.split('/').enumerate() {
            if i < 2 {
                byte_offset += seg.len() + 1; // segment + '/'
            } else {
                return &self.0[byte_offset..];
            }
        }
        // Unreachable: validator guarantees ≥3 segments.
        ""
    }

    /// Iterate over the path's `/`-separated segments.
    pub fn segments(&self) -> impl Iterator<Item = &str> {
        self.0.split('/')
    }

    /// `true` if the path is in one of the reserved framework
    /// namespaces (`__*` or `_test/*`). User-facing surfaces hide
    /// such paths by default; see ADR-020 §2 and ADR-021 §5.
    pub fn is_internal(&self) -> bool {
        is_reserved_prefix(self.scope())
    }

    fn segment(&self, idx: usize) -> &str {
        self.0.split('/').nth(idx).unwrap_or("")
    }
}

impl fmt::Display for SecretPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for SecretPath {
    type Err = PathError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

impl AsRef<str> for SecretPath {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl Serialize for SecretPath {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for SecretPath {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        SecretPath::parse(&s).map_err(serde::de::Error::custom)
    }
}

// =============================================================================
// Internal validation
// =============================================================================

fn validate(s: &str) -> Result<(), PathError> {
    validate_with(s, /* allow_reserved = */ false)
}

fn validate_with(s: &str, allow_reserved: bool) -> Result<(), PathError> {
    let segments: Vec<&str> = s.split('/').collect();

    if segments.len() < 3 {
        return Err(PathError::TooFewSegments {
            path: s.to_owned(),
            found: segments.len(),
        });
    }

    // Empty-segment check runs first so the rest of the loop can rely on
    // non-empty bytes when classifying each position.
    for (idx, seg) in segments.iter().enumerate() {
        if seg.is_empty() {
            return Err(PathError::EmptySegment {
                path: s.to_owned(),
                index: idx,
            });
        }
    }

    // Position 0 is the scope. It is either one of the two well-formed
    // reserved prefixes (`__<kebab>` or exactly `_test`) — admitted only
    // through the `parse_internal` entry point — or a regular kebab
    // segment. Reserved-classification has to run *before* the kebab
    // check because reserved segments begin with `_`, which the kebab
    // alphabet rejects.
    let scope = segments[0];
    if is_reserved_segment(scope) {
        if !allow_reserved {
            return Err(PathError::ReservedPrefix {
                path: s.to_owned(),
                prefix: scope.to_owned(),
            });
        }
        // else: reserved scope is permitted; fall through to validate
        // the remaining segments.
    } else if !is_kebab_segment(scope) {
        return Err(PathError::BadSegment {
            path: s.to_owned(),
            segment: scope.to_owned(),
            index: 0,
        });
    }

    for (idx, seg) in segments.iter().enumerate().skip(1) {
        if !is_kebab_segment(seg) {
            return Err(PathError::BadSegment {
                path: s.to_owned(),
                segment: (*seg).to_owned(),
                index: idx,
            });
        }
    }

    Ok(())
}

/// Check `[a-z][a-z0-9-]*` without pulling in the `regex` crate. Hot
/// path on every secret resolution; explicit ASCII byte loop is the
/// cheapest implementation and keeps this crate dependency-light.
fn is_kebab_segment(seg: &str) -> bool {
    let bytes = seg.as_bytes();
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

/// Loose check used by [`SecretPath::is_internal`] on already-validated
/// paths. The strict per-form classifier is [`is_reserved_segment`].
fn is_reserved_prefix(scope: &str) -> bool {
    scope.starts_with(RESERVED_INTERNAL_PREFIX) || scope == RESERVED_TEST_PREFIX
}

/// Strict classifier for a *well-formed* reserved scope segment. Used at
/// validation time to decide whether to apply [`PathError::ReservedPrefix`]
/// (instead of the generic kebab-rejection) and whether the path is
/// admissible through [`SecretPath::parse_internal`].
fn is_reserved_segment(seg: &str) -> bool {
    if seg == RESERVED_TEST_PREFIX {
        return true;
    }
    if let Some(rest) = seg.strip_prefix(RESERVED_INTERNAL_PREFIX) {
        return is_kebab_segment(rest);
    }
    false
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // -- Valid paths -----------------------------------------------------------

    #[test]
    fn accepts_three_segment_path() {
        let p = SecretPath::parse("team/gitlab/token-deploy").unwrap();
        assert_eq!(p.as_str(), "team/gitlab/token-deploy");
        assert_eq!(p.scope(), "team");
        assert_eq!(p.provider(), "gitlab");
        assert_eq!(p.purpose(), "token-deploy");
        assert!(!p.is_internal());
    }

    #[test]
    fn accepts_canonical_examples_from_adr_020() {
        for s in [
            "team/gitlab/token-deploy",
            "team/openai/api-key",
            "personal/github/pat",
            "personal/anthropic/api-key",
            "client-acme/jira/api-key",
            "sandbox/example-provider/token",
        ] {
            assert!(SecretPath::parse(s).is_ok(), "{s} should parse");
        }
    }

    #[test]
    fn accepts_paths_longer_than_three_segments_in_purpose() {
        let p = SecretPath::parse("team/gitlab/ci/deploy/token").unwrap();
        assert_eq!(p.scope(), "team");
        assert_eq!(p.provider(), "gitlab");
        assert_eq!(p.purpose(), "ci/deploy/token");
        assert_eq!(p.segments().count(), 5);
    }

    #[test]
    fn parse_via_fromstr_trait() {
        let p: SecretPath = "personal/github/pat".parse().unwrap();
        assert_eq!(p.as_str(), "personal/github/pat");
    }

    // -- TooFewSegments --------------------------------------------------------

    #[test]
    fn rejects_single_segment() {
        match SecretPath::parse("token") {
            Err(PathError::TooFewSegments { found, .. }) => assert_eq!(found, 1),
            other => panic!("expected TooFewSegments, got {other:?}"),
        }
    }

    #[test]
    fn rejects_two_segments() {
        let err = SecretPath::parse("team/gitlab").unwrap_err();
        assert!(matches!(err, PathError::TooFewSegments { found: 2, .. }));
    }

    #[test]
    fn rejects_dot_separator_as_too_few_segments() {
        // `gitlab.token` has no `/`, so it parses as a single segment.
        let err = SecretPath::parse("gitlab.token").unwrap_err();
        assert!(matches!(err, PathError::TooFewSegments { found: 1, .. }));
    }

    // -- EmptySegment ----------------------------------------------------------

    #[test]
    fn rejects_empty_middle_segment() {
        let err = SecretPath::parse("team//gitlab/token").unwrap_err();
        match err {
            PathError::EmptySegment { index, .. } => assert_eq!(index, 1),
            other => panic!("expected EmptySegment, got {other:?}"),
        }
    }

    #[test]
    fn rejects_empty_leading_segment() {
        let err = SecretPath::parse("/gitlab/token/deploy").unwrap_err();
        assert!(matches!(err, PathError::EmptySegment { index: 0, .. }));
    }

    #[test]
    fn rejects_empty_trailing_segment() {
        let err = SecretPath::parse("team/gitlab/token/").unwrap_err();
        // 4 segments, last empty (index 3).
        assert!(matches!(err, PathError::EmptySegment { index: 3, .. }));
    }

    // -- BadSegment ------------------------------------------------------------

    #[test]
    fn rejects_uppercase_segment() {
        let err = SecretPath::parse("Team/gitlab/token").unwrap_err();
        match err {
            PathError::BadSegment { segment, index, .. } => {
                assert_eq!(segment, "Team");
                assert_eq!(index, 0);
            }
            other => panic!("expected BadSegment, got {other:?}"),
        }
    }

    #[test]
    fn rejects_kebab_starting_with_digit() {
        let err = SecretPath::parse("team/9gitlab/token").unwrap_err();
        assert!(matches!(err, PathError::BadSegment { index: 1, .. }));
    }

    #[test]
    fn rejects_kebab_starting_with_dash() {
        let err = SecretPath::parse("team/-gitlab/token").unwrap_err();
        assert!(matches!(err, PathError::BadSegment { index: 1, .. }));
    }

    #[test]
    fn rejects_segment_with_underscore() {
        let err = SecretPath::parse("team/gitlab/token_deploy").unwrap_err();
        assert!(matches!(err, PathError::BadSegment { index: 2, .. }));
    }

    #[test]
    fn rejects_segment_with_dot() {
        let err = SecretPath::parse("team/gitlab.token/deploy").unwrap_err();
        assert!(matches!(err, PathError::BadSegment { index: 1, .. }));
    }

    // -- ReservedPrefix --------------------------------------------------------

    #[test]
    fn rejects_double_underscore_prefix() {
        let err = SecretPath::parse("__sources/vault-a/token").unwrap_err();
        match err {
            PathError::ReservedPrefix { prefix, .. } => {
                assert_eq!(prefix, "__sources");
            }
            other => panic!("expected ReservedPrefix, got {other:?}"),
        }
    }

    #[test]
    fn rejects_underscore_test_prefix() {
        let err = SecretPath::parse("_test/foo/bar").unwrap_err();
        assert!(matches!(err, PathError::ReservedPrefix { .. }));
    }

    #[test]
    fn parse_internal_accepts_double_underscore_prefix() {
        let p = SecretPath::parse_internal("__sources/vault-team/deploy").unwrap();
        assert_eq!(p.scope(), "__sources");
        assert!(p.is_internal());
    }

    #[test]
    fn parse_internal_accepts_test_prefix() {
        let p = SecretPath::parse_internal("_test/example/secret").unwrap();
        assert!(p.is_internal());
    }

    #[test]
    fn parse_internal_still_rejects_bad_segments() {
        // Reserved prefix is allowed, but the rest of the path still has to be
        // valid kebab-case.
        let err = SecretPath::parse_internal("__sources/Vault/token").unwrap_err();
        assert!(matches!(err, PathError::BadSegment { index: 1, .. }));
    }

    // -- Display / accessors ---------------------------------------------------

    #[test]
    fn display_returns_full_path() {
        let p = SecretPath::parse("team/gitlab/token-deploy").unwrap();
        assert_eq!(format!("{p}"), "team/gitlab/token-deploy");
    }

    #[test]
    fn segments_iter_returns_each_part() {
        let p = SecretPath::parse("team/gitlab/token-deploy").unwrap();
        let segs: Vec<_> = p.segments().collect();
        assert_eq!(segs, vec!["team", "gitlab", "token-deploy"]);
    }

    #[test]
    fn as_ref_str_works() {
        let p = SecretPath::parse("team/gitlab/token-deploy").unwrap();
        let s: &str = p.as_ref();
        assert_eq!(s, "team/gitlab/token-deploy");
    }
}
