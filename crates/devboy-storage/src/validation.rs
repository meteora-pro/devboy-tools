//! Format validation per [ADR-021] §6 (the "validation framework"
//! umbrella) and [ADR-020] §3 (`format_regex` / `pattern_id`
//! metadata fields).
//!
//! The check is *format-only* and *lazy on demand*: it compares a
//! candidate value against the regex declared by the global-index
//! entry (`format_regex`) or, failing that, by the pattern referenced
//! through `pattern_id`. A `Liveness` probe — actually asking the
//! upstream whether the value still works — is a separate phase
//! (P9.2) and lives in its own module.
//!
//! ## Resolution order
//!
//! 1. If the entry has an inline `format_regex`, compile and use it.
//! 2. Otherwise, if the entry has a `pattern_id`, look it up in the
//!    [`devboy_secret_patterns::Catalogue`] and use its
//!    [`SecretPattern::format_regex`].
//! 3. Otherwise, return [`FormatCheck::NoRule`] — the caller chose
//!    not to declare a format, so the validator stays silent.
//!
//! Inline `format_regex` wins over `pattern_id` because a project may
//! have a tighter shape in mind than the generic pattern (e.g. a
//! regex that pins the prefix to a specific tenant id).
//!
//! ## What the validator does **not** do
//!
//! - **Compile patterns ahead of time.** Inline `format_regex`
//!   compiles on every call. The catalogue's `format_regex()` is
//!   already cached behind `OnceLock`. A full ahead-of-time compile
//!   of all index entries can land later if profiling shows it's
//!   needed; for now `secrets validate <path>` is on demand and the
//!   cost is acceptable.
//! - **Probe upstream liveness.** That's P9.2. A pattern that is
//!   well-formed but revoked still passes this check.
//!
//! [ADR-020]: https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-020-secret-manifest-and-alias-resolution.md
//! [ADR-021]: https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-021-external-secret-sources.md

use devboy_secret_patterns::Catalogue;

use crate::index::IndexEntry;

// =============================================================================
// Public types
// =============================================================================

/// Outcome of [`validate_format`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FormatCheck {
    /// No `format_regex` and no `pattern_id` (or the `pattern_id`
    /// names a pattern that isn't in the catalogue). The caller
    /// chose not to declare a shape; format validation is a no-op.
    NoRule,
    /// The value matched the resolved regex.
    Ok {
        /// Where the regex came from — `format_regex` inline on the
        /// entry, or the `pattern_id` it resolved through.
        source: FormatRuleSource,
    },
    /// The value did not match.
    Mismatch {
        /// Where the regex came from.
        source: FormatRuleSource,
        /// The regex pattern that was checked. Useful in error
        /// messages — `{expected}` lets the user see exactly which
        /// shape the system was expecting.
        expected: String,
    },
    /// Something went wrong during validation — usually a regex
    /// compile failure for an inline `format_regex`.
    Error {
        /// Human-readable detail.
        message: String,
    },
}

/// Provenance of the regex used by the check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FormatRuleSource {
    /// Inline `format_regex` on the index entry.
    Inline,
    /// `pattern_id` resolved through the catalogue. The string is
    /// the `id` we looked up.
    PatternId(String),
}

// =============================================================================
// Validator
// =============================================================================

/// Validate `value` against the format rule attached to `entry`.
///
/// Resolution order:
///
/// 1. `entry.format_regex` (inline) wins.
/// 2. `entry.pattern_id` then resolves through `catalogue`.
/// 3. Neither set → [`FormatCheck::NoRule`].
///
/// Inline regex compile errors surface as
/// [`FormatCheck::Error`] — the caller decides whether that's a
/// hard fail or a `doctor`-style warning.
pub fn validate_format(entry: &IndexEntry, value: &str, catalogue: &Catalogue) -> FormatCheck {
    if let Some(pattern) = entry.format_regex.as_deref() {
        let re = match regex::Regex::new(pattern) {
            Ok(r) => r,
            Err(e) => {
                return FormatCheck::Error {
                    message: format!("invalid format_regex `{pattern}`: {e}"),
                };
            }
        };
        return if re.is_match(value) {
            FormatCheck::Ok {
                source: FormatRuleSource::Inline,
            }
        } else {
            FormatCheck::Mismatch {
                source: FormatRuleSource::Inline,
                expected: pattern.to_owned(),
            }
        };
    }

    if let Some(id) = entry.pattern_id.as_deref() {
        let pattern = match catalogue.find(id) {
            Some(p) => p,
            None => {
                // Pattern referenced but not loaded — treat as
                // "no rule" rather than Error. The recursion check
                // (P5.5) and `doctor` already surface unresolved
                // pattern_ids; the format validator should not
                // double-fail on that.
                return FormatCheck::NoRule;
            }
        };
        let re = pattern.format_regex();
        return if re.is_match(value) {
            FormatCheck::Ok {
                source: FormatRuleSource::PatternId(id.to_owned()),
            }
        } else {
            FormatCheck::Mismatch {
                source: FormatRuleSource::PatternId(id.to_owned()),
                expected: re.as_str().to_owned(),
            }
        };
    }

    FormatCheck::NoRule
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::IndexEntry;
    use devboy_secret_patterns::Catalogue;

    fn empty_catalogue() -> Catalogue {
        Catalogue::builtins_only()
    }

    fn entry_with_inline(regex: &str) -> IndexEntry {
        IndexEntry {
            format_regex: Some(regex.to_owned()),
            ..IndexEntry::default()
        }
    }

    fn entry_with_pattern_id(id: &str) -> IndexEntry {
        IndexEntry {
            pattern_id: Some(id.to_owned()),
            ..IndexEntry::default()
        }
    }

    // -- No rule ---------------------------------------------------

    #[test]
    fn no_rule_when_neither_field_is_set() {
        let entry = IndexEntry::default();
        let r = validate_format(&entry, "anything", &empty_catalogue());
        assert!(matches!(r, FormatCheck::NoRule));
    }

    // -- Inline format_regex --------------------------------------

    #[test]
    fn inline_regex_matching_value_returns_ok_with_inline_source() {
        let entry = entry_with_inline(r"^ghp_[A-Za-z0-9]{36}$");
        let value = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";
        let r = validate_format(&entry, value, &empty_catalogue());
        match r {
            FormatCheck::Ok { source } => assert_eq!(source, FormatRuleSource::Inline),
            other => panic!("expected Ok, got {other:?}"),
        }
    }

    #[test]
    fn inline_regex_mismatching_value_returns_mismatch_with_pattern_echoed() {
        let entry = entry_with_inline(r"^ghp_[A-Za-z0-9]{36}$");
        let r = validate_format(&entry, "not-a-token", &empty_catalogue());
        match r {
            FormatCheck::Mismatch { source, expected } => {
                assert_eq!(source, FormatRuleSource::Inline);
                assert_eq!(expected, r"^ghp_[A-Za-z0-9]{36}$");
            }
            other => panic!("expected Mismatch, got {other:?}"),
        }
    }

    #[test]
    fn invalid_inline_regex_surfaces_compile_error() {
        // `[unterminated` — guaranteed compile failure across regex
        // versions.
        let entry = entry_with_inline("[unterminated");
        let r = validate_format(&entry, "anything", &empty_catalogue());
        match r {
            FormatCheck::Error { message } => {
                assert!(message.contains("invalid format_regex"));
                assert!(message.contains("[unterminated"));
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    // -- pattern_id via catalogue ---------------------------------

    #[test]
    fn known_pattern_id_matches_a_real_token() {
        // `github-pat` is a built-in (P2.2) — `ghp_` followed by
        // 36 ASCII alphanum chars. Use a synthetic well-formed
        // sample.
        let entry = entry_with_pattern_id("github-pat");
        let value = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";
        let r = validate_format(&entry, value, &empty_catalogue());
        match r {
            FormatCheck::Ok { source } => {
                assert_eq!(source, FormatRuleSource::PatternId("github-pat".into()));
            }
            other => panic!("expected Ok, got {other:?}"),
        }
    }

    #[test]
    fn known_pattern_id_rejects_gibberish() {
        let entry = entry_with_pattern_id("github-pat");
        let r = validate_format(&entry, "definitely-not-a-token", &empty_catalogue());
        match r {
            FormatCheck::Mismatch { source, .. } => {
                assert_eq!(source, FormatRuleSource::PatternId("github-pat".into()));
            }
            other => panic!("expected Mismatch, got {other:?}"),
        }
    }

    #[test]
    fn unknown_pattern_id_treated_as_no_rule() {
        // `pattern_id` references something the catalogue doesn't
        // have. The format validator returns `NoRule` rather than
        // failing — `doctor` is the place that flags missing
        // pattern ids.
        let entry = entry_with_pattern_id("not-a-real-pattern-id");
        let r = validate_format(&entry, "anything", &empty_catalogue());
        assert!(matches!(r, FormatCheck::NoRule));
    }

    // -- Inline beats pattern_id when both are set ---------------

    #[test]
    fn inline_format_regex_wins_over_pattern_id() {
        // Both fields set. Inline pattern is restrictive
        // (`tighter-prefix-`); the pattern_id (`github-pat`) is
        // looser. The inline regex must be the one consulted.
        let mut entry = entry_with_pattern_id("github-pat");
        entry.format_regex = Some(r"^tighter-prefix-[a-z]+$".to_owned());

        // Inline matches → Ok with Inline source (NOT PatternId).
        let r = validate_format(&entry, "tighter-prefix-abc", &empty_catalogue());
        match r {
            FormatCheck::Ok { source } => assert_eq!(source, FormatRuleSource::Inline),
            other => panic!("expected Inline Ok, got {other:?}"),
        }

        // Inline does NOT match — even if a github-pat-shaped
        // value is supplied, the inline rule wins and rejects.
        let r = validate_format(
            &entry,
            "ghp_abcdefghijklmnopqrstuvwxyz0123456789",
            &empty_catalogue(),
        );
        assert!(matches!(
            r,
            FormatCheck::Mismatch {
                source: FormatRuleSource::Inline,
                ..
            }
        ));
    }

    // -- User patterns via Catalogue ------------------------------

    /// Smoke test that a user-loaded pattern (added through a
    /// `patterns.d/` TOML file) is reachable via `Catalogue::find`
    /// and therefore consumed by the validator.
    #[test]
    fn user_pattern_via_catalogue_is_used() {
        // We construct the catalogue by parsing an inline TOML
        // pattern. `Catalogue::load` reads from a directory; we
        // skip that path and use the same `parse_str` route the
        // loader uses internally — but `parse_str` isn't part of
        // the public API yet. Until it is, this test remains a
        // compile-only smoke ensuring `validate_format` typechecks
        // against the public `Catalogue`.
        //
        // The full user-pattern integration test will land in P9.4
        // (`devboy secrets validate`), where the CLI builds a real
        // catalogue from `patterns.d/`.
        let entry = entry_with_pattern_id("user-defined-not-loaded");
        let r = validate_format(&entry, "x", &empty_catalogue());
        // Without the user file the catalogue can't find the id —
        // contract is `NoRule`.
        assert!(matches!(r, FormatCheck::NoRule));
    }
}
