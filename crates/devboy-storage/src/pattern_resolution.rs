//! Pattern-id inheritance per [ADR-020] §3 + [ADR-023] §3.6.
//!
//! When an [`IndexEntry`] sets `pattern_id`, the linked entry in the
//! shared [`Catalogue`] supplies sensible defaults for fields the
//! entry left blank: the format regex, the retrieval URL, the rotation
//! cadence, the rotation method. This module is the wiring that turns
//! "entry references a pattern" into "entry has fully-resolved
//! metadata".
//!
//! # Inheritance is one-way
//!
//! - Explicit fields on the entry **always** win.
//! - Pattern defaults fill in **only** when the corresponding entry
//!   field is `None`.
//! - When `pattern_id` references an id that does not exist in the
//!   catalogue, the entry is returned unchanged and an
//!   [`InheritanceWarning::UnknownPatternId`] is recorded.
//!   Surfacing the missing id is `doctor`'s job (epic phase P7.3);
//!   the resolver itself does not error.
//!
//! # Field mapping
//!
//! | `IndexEntry` field       | Pattern source                                         |
//! |--------------------------|---------------------------------------------------------|
//! | `format_regex`           | `SecretPattern::format_regex().as_str()`               |
//! | `retrieval_url`          | `SecretPattern::metadata()?.retrieval_url_template`    |
//! | `rotate_every_days`      | `SecretPattern::metadata()?.default_expiry_days`       |
//! | `rotation_method`        | `SecretPattern::rotation()?.method` (mapped)           |
//!
//! Other entry fields (`description`, `default_gate`, `expires_at`,
//! `last_rotated_at`, `required_scopes`, `env_var`,
//! `cache_ttl_seconds_max`) are not pattern-driven and pass through
//! unchanged.
//!
//! [ADR-020]: https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-020-secret-manifest-and-alias-resolution.md
//! [ADR-023]: https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-023-secret-store-ux-layer.md

use devboy_secret_patterns::{Catalogue, RotationMethodSpec, SecretPattern};

use crate::index::{IndexEntry, RotationMethod};

/// Non-fatal advisory about pattern-id inheritance, surfaced through
/// `doctor` (epic phase P7.3) so the user can fix manifest hygiene
/// issues without the resolver itself failing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InheritanceWarning {
    /// What the warning is about.
    pub kind: InheritanceWarningKind,
    /// The `pattern_id` value that triggered the warning.
    pub pattern_id: String,
}

/// Categories of advisory warnings produced by [`apply_pattern_inheritance`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InheritanceWarningKind {
    /// `pattern_id` does not match any built-in or user-supplied
    /// pattern. The entry is returned unchanged; the user should
    /// either spell-check the id or register the pattern under
    /// `~/.devboy/secrets/patterns.d/`.
    UnknownPatternId,
}

/// Apply pattern-id inheritance to `entry` and return the resolved
/// entry plus any advisory warning.
///
/// The function clones `entry` so the caller's view is unchanged.
/// Cloning is cheap (the entry is a handful of `Option<String>` /
/// small primitive fields) and the alternative — mutating in place —
/// would force every caller to express the lifetime of the catalogue
/// borrow, which is more friction than it's worth here.
pub fn apply_pattern_inheritance(
    entry: &IndexEntry,
    catalogue: &Catalogue,
) -> (IndexEntry, Option<InheritanceWarning>) {
    let resolved = entry.clone();
    let Some(id) = entry.pattern_id.as_deref() else {
        return (resolved, None);
    };
    let Some(pattern) = catalogue.find(id) else {
        return (
            resolved,
            Some(InheritanceWarning {
                kind: InheritanceWarningKind::UnknownPatternId,
                pattern_id: id.to_owned(),
            }),
        );
    };
    let resolved = inherit_from_pattern(resolved, pattern);
    (resolved, None)
}

fn inherit_from_pattern(mut entry: IndexEntry, pattern: &dyn SecretPattern) -> IndexEntry {
    if entry.format_regex.is_none() {
        entry.format_regex = Some(pattern.format_regex().as_str().to_owned());
    }
    if let Some(meta) = pattern.metadata() {
        if entry.retrieval_url.is_none() {
            entry.retrieval_url = Some(meta.retrieval_url_template.to_string());
        }
        if entry.rotate_every_days.is_none() {
            if let Some(d) = meta.default_expiry_days {
                entry.rotate_every_days = Some(d);
            }
        }
    }
    if let Some(rotation) = pattern.rotation() {
        if entry.rotation_method.is_none() {
            entry.rotation_method = Some(map_rotation_method(&rotation.method));
        }
    }
    entry
}

/// Translate the catalogue's [`RotationMethodSpec`] into the storage
/// crate's [`RotationMethod`]. They are kept as separate enums on
/// purpose: the catalogue variant carries an extra URL template that
/// the storage TOML representation does not need.
fn map_rotation_method(m: &RotationMethodSpec) -> RotationMethod {
    match m {
        RotationMethodSpec::Manual => RotationMethod::Manual,
        RotationMethodSpec::ProviderUi { .. } => RotationMethod::ProviderUi,
        RotationMethodSpec::ProviderApi => RotationMethod::ProviderApi,
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::{Gate, IndexEntry, RotationMethod};

    fn entry_with_pattern(id: &str) -> IndexEntry {
        IndexEntry {
            pattern_id: Some(id.to_owned()),
            ..IndexEntry::default()
        }
    }

    #[test]
    fn entry_without_pattern_id_passes_through_unchanged() {
        let cat = Catalogue::builtins_only();
        let entry = IndexEntry {
            description: Some("explicit".to_owned()),
            ..IndexEntry::default()
        };
        let (resolved, warning) = apply_pattern_inheritance(&entry, &cat);
        assert_eq!(resolved, entry);
        assert!(warning.is_none());
    }

    #[test]
    fn unknown_pattern_id_returns_entry_and_warning() {
        let cat = Catalogue::builtins_only();
        let entry = entry_with_pattern("no-such-pattern");
        let (resolved, warning) = apply_pattern_inheritance(&entry, &cat);
        assert_eq!(resolved, entry, "entry must be returned unchanged");
        let w = warning.expect("must produce a warning");
        assert_eq!(w.kind, InheritanceWarningKind::UnknownPatternId);
        assert_eq!(w.pattern_id, "no-such-pattern");
    }

    #[test]
    fn known_pattern_inherits_format_regex() {
        // `github-pat` is built-in, no metadata override needed.
        let cat = Catalogue::builtins_only();
        let entry = entry_with_pattern("github-pat");
        let (resolved, warning) = apply_pattern_inheritance(&entry, &cat);
        assert!(warning.is_none());
        let regex = resolved.format_regex.expect("regex inherited");
        assert!(regex.starts_with('^'));
        assert!(regex.contains("gh"));
    }

    #[test]
    fn explicit_format_regex_is_not_overridden() {
        let cat = Catalogue::builtins_only();
        let entry = IndexEntry {
            pattern_id: Some("github-pat".to_owned()),
            format_regex: Some("^my-explicit-regex$".to_owned()),
            ..IndexEntry::default()
        };
        let (resolved, _) = apply_pattern_inheritance(&entry, &cat);
        assert_eq!(
            resolved.format_regex.as_deref(),
            Some("^my-explicit-regex$")
        );
    }

    #[test]
    fn known_pattern_inherits_retrieval_url() {
        let cat = Catalogue::builtins_only();
        let entry = entry_with_pattern("github-pat");
        let (resolved, _) = apply_pattern_inheritance(&entry, &cat);
        assert_eq!(
            resolved.retrieval_url.as_deref(),
            Some("https://github.com/settings/tokens")
        );
    }

    #[test]
    fn explicit_retrieval_url_is_not_overridden() {
        let cat = Catalogue::builtins_only();
        let entry = IndexEntry {
            pattern_id: Some("github-pat".to_owned()),
            retrieval_url: Some("https://internal.example/tokens".to_owned()),
            ..IndexEntry::default()
        };
        let (resolved, _) = apply_pattern_inheritance(&entry, &cat);
        assert_eq!(
            resolved.retrieval_url.as_deref(),
            Some("https://internal.example/tokens")
        );
    }

    #[test]
    fn known_pattern_inherits_rotate_every_days() {
        // Built-in `github-pat` carries `default_expiry_days = 90`.
        let cat = Catalogue::builtins_only();
        let entry = entry_with_pattern("github-pat");
        let (resolved, _) = apply_pattern_inheritance(&entry, &cat);
        assert_eq!(resolved.rotate_every_days, Some(90));
    }

    #[test]
    fn explicit_rotate_every_days_is_not_overridden() {
        let cat = Catalogue::builtins_only();
        let entry = IndexEntry {
            pattern_id: Some("github-pat".to_owned()),
            rotate_every_days: Some(30),
            ..IndexEntry::default()
        };
        let (resolved, _) = apply_pattern_inheritance(&entry, &cat);
        assert_eq!(resolved.rotate_every_days, Some(30));
    }

    #[test]
    fn pattern_without_metadata_only_inherits_regex() {
        // `jwt` is a built-in with no metadata layer — only the
        // regex should be inherited.
        let cat = Catalogue::builtins_only();
        let entry = entry_with_pattern("jwt");
        let (resolved, _) = apply_pattern_inheritance(&entry, &cat);
        assert!(resolved.format_regex.is_some(), "regex must inherit");
        assert!(
            resolved.retrieval_url.is_none(),
            "no metadata → no retrieval url"
        );
        assert!(
            resolved.rotate_every_days.is_none(),
            "no metadata → no expiry default"
        );
    }

    #[test]
    fn unrelated_fields_pass_through_unchanged() {
        let cat = Catalogue::builtins_only();
        let entry = IndexEntry {
            pattern_id: Some("github-pat".to_owned()),
            description: Some("My deploy token".to_owned()),
            default_gate: Some(Gate::Touchid),
            expires_at: Some("2026-08-01".to_owned()),
            last_rotated_at: Some("2026-05-02".to_owned()),
            required_scopes: vec!["repo".to_owned()],
            env_var: Some("GH_TOKEN".to_owned()),
            cache_ttl_seconds_max: Some(60),
            ..IndexEntry::default()
        };
        let (resolved, _) = apply_pattern_inheritance(&entry, &cat);
        // Pattern-driven fields filled in.
        assert!(resolved.format_regex.is_some());
        assert!(resolved.retrieval_url.is_some());
        assert_eq!(resolved.rotate_every_days, Some(90));
        // Non-pattern fields untouched.
        assert_eq!(resolved.description.as_deref(), Some("My deploy token"));
        assert_eq!(resolved.default_gate, Some(Gate::Touchid));
        assert_eq!(resolved.expires_at.as_deref(), Some("2026-08-01"));
        assert_eq!(resolved.last_rotated_at.as_deref(), Some("2026-05-02"));
        assert_eq!(resolved.required_scopes, vec!["repo"]);
        assert_eq!(resolved.env_var.as_deref(), Some("GH_TOKEN"));
        assert_eq!(resolved.cache_ttl_seconds_max, Some(60));
    }

    #[test]
    fn rotation_method_remains_none_for_v1_builtins() {
        // No built-in carries a `rotation` spec in v1 (P2.2 leaves
        // it unset; provider-driven rotation is deferred per
        // ADR-023 §3.5). Verify the inheritance helper does not
        // synthesise a value out of thin air.
        let cat = Catalogue::builtins_only();
        let entry = entry_with_pattern("github-pat");
        let (resolved, _) = apply_pattern_inheritance(&entry, &cat);
        assert_eq!(
            resolved.rotation_method, None,
            "no built-in supplies a rotation spec yet"
        );
    }

    #[test]
    fn map_rotation_method_covers_each_variant() {
        // Compile-time exhaustiveness check via `match` plus
        // explicit asserts — the storage enum and the patterns
        // enum can drift; this catches that.
        assert_eq!(
            map_rotation_method(&RotationMethodSpec::Manual),
            RotationMethod::Manual
        );
        assert_eq!(
            map_rotation_method(&RotationMethodSpec::ProviderUi {
                url_template: "https://example/r"
            }),
            RotationMethod::ProviderUi
        );
        assert_eq!(
            map_rotation_method(&RotationMethodSpec::ProviderApi),
            RotationMethod::ProviderApi
        );
    }
}
