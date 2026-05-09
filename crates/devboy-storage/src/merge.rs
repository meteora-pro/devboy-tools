//! Manifest-with-global-index merge per [ADR-020] §4.
//!
//! Given a global index ([`GlobalIndex`]) and a per-project manifest
//! ([`ProjectManifest`]), the merge function produces a single
//! resolved view per declared path: which entry "wins", which fields
//! came from the manifest's `[overrides]` block, and what the final
//! metadata is.
//!
//! # Precedence
//!
//! For each path mentioned in the manifest's `required` or `optional`
//! list, the resolver applies precedence rules straight from ADR-020 §4:
//!
//! 1. If the path appears as `[secret."<path>"]` in the manifest, its
//!    metadata is read from the manifest only — the path is
//!    project-local.
//! 2. Otherwise, metadata is read from the global index. If the
//!    manifest has an `[overrides."<path>"]` block, the listed
//!    behavioural fields override the index values.
//! 3. If a path appears in `required` / `optional` and exists in
//!    **neither** the global index nor as `[secret."..."]`, the
//!    merge fails with [`MergeError::UnknownPath`] — the
//!    `E_SECRET_UNKNOWN_PATH` error from the ADR.
//!
//! `E_OVERRIDE_FIELD_NOT_ALLOWED` (the ADR's name for "you tried to
//! override a non-behavioural field") is already enforced at *parse*
//! time by `deny_unknown_fields` on [`OverrideEntry`] — it surfaces
//! through [`ManifestError::Parse`](crate::manifest::ManifestError::Parse)
//! during loading, not here.
//!
//! # Warnings (advisory)
//!
//! The merge also emits non-fatal warnings, returned alongside the
//! resolved view. `doctor` consumes them; the loader itself accepts
//! the manifest. Cases:
//!
//! - [`MergeWarning::NoOpOverride`] — an `[overrides]` field is set to
//!   the same value as the global. The override is harmless but adds
//!   noise; suggest removal.
//! - [`MergeWarning::OverrideForUndeclaredPath`] — an `[overrides."x"]`
//!   block exists for a path that is not in `required` / `optional`.
//!   Possibly a typo or a leftover; the override is unused.
//! - [`MergeWarning::ProjectLocalForUndeclaredPath`] — a
//!   `[secret."x"]` block exists for a path that is not in
//!   `required` / `optional`. Same shape as the previous warning;
//!   metadata is registered but no caller depends on it.
//!
//! [ADR-020]: https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-020-secret-manifest-and-alias-resolution.md

use std::collections::BTreeMap;

use serde::Serialize;
use thiserror::Error;

use crate::index::{GlobalIndex, IndexEntry};
use crate::manifest::{OverrideEntry, PathRole, ProjectManifest};
use crate::secret_path::SecretPath;

/// Which behavioural field of a global entry was replaced by the
/// project's `[overrides]` block.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OverrideField {
    /// The `gate` field.
    Gate,
    /// The `rotate_every_days` field.
    RotateEveryDays,
    /// The `description` field.
    Description,
}

/// Where the resolved metadata for a path came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecretOrigin {
    /// The path's metadata lives entirely in the project manifest's
    /// `[secret."<path>"]` block (rule 1 of the precedence).
    ProjectLocal,
    /// The path's metadata came from the global index, optionally with
    /// behavioural overrides applied (rule 2 of the precedence).
    Global {
        /// Which override fields were actually applied. Empty when
        /// the path has no `[overrides]` block.
        overrides_applied: Vec<OverrideField>,
    },
}

/// Resolved view of a single declared path — final metadata plus
/// provenance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedSecret {
    /// The path (validated through ADR-020 §2 by the loaders that
    /// produced it).
    pub path: SecretPath,
    /// `true` when the path was declared in the manifest's `required`
    /// list; `false` when it was in `optional`.
    pub required: bool,
    /// Where the resolved metadata came from.
    pub origin: SecretOrigin,
    /// Final, post-override metadata.
    pub metadata: IndexEntry,
}

/// Output of [`merge_manifest`] — the resolved view plus advisory
/// warnings.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct MergeOutput {
    /// One [`ResolvedSecret`] per declared path, sorted by path.
    pub secrets: BTreeMap<SecretPath, ResolvedSecret>,
    /// Non-fatal warnings produced during the merge.
    pub warnings: Vec<MergeWarning>,
}

/// Advisory warning emitted by the merge.
///
/// Warnings do not fail the merge; they are surfaced through `doctor`
/// to nudge the user toward fixing manifest hygiene issues.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MergeWarning {
    /// What the warning is about.
    pub kind: MergeWarningKind,
    /// Which path the warning concerns.
    pub path: SecretPath,
}

/// Categories of advisory warnings — see the module-level doc for
/// detailed descriptions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MergeWarningKind {
    /// An `[overrides]` field carries the same value as the global
    /// index. The override is a no-op; recommend removing it so the
    /// project does not silently fall behind a future global change.
    NoOpOverride {
        /// Which field is identical.
        field: OverrideField,
    },
    /// An `[overrides."<path>"]` block exists for a path that is not
    /// listed in `required` or `optional`. Likely a typo or leftover.
    OverrideForUndeclaredPath,
    /// A `[secret."<path>"]` block exists for a path that is not
    /// listed in `required` or `optional`. The metadata is registered
    /// but no caller depends on it.
    ProjectLocalForUndeclaredPath,
}

/// Fatal merge errors.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum MergeError {
    /// The path appears in `required` or `optional` but has no entry
    /// in either the global index or the manifest's `[secret."..."]`
    /// blocks. ADR-020 §4 calls this `E_SECRET_UNKNOWN_PATH`.
    #[error(
        "secret path '{path}' is declared in {role} but no metadata is registered for it; \
         add a `[secret.\"{path}\"]` block to the manifest or register it in the global index \
         (run `devboy secrets describe {path}` for guidance)"
    )]
    UnknownPath {
        /// The undeclared path.
        path: SecretPath,
        /// Whether it was in `required` or `optional`.
        role: PathRole,
    },

    /// An `[overrides."<path>"]` block targets a path that is also
    /// declared as `[secret."<path>"]` (project-local). The two are
    /// in conflict: project-local entries already carry the full
    /// metadata, and an override on top of them silently mixes
    /// concerns. Spec rule 1 wins (`secret` block) but the merge
    /// rejects this configuration outright so drift cannot grow
    /// silently.
    #[error(
        "secret path '{path}' has both an `[overrides.\"{path}\"]` block and a project-local \
         `[secret.\"{path}\"]` block; remove the override (project-local entries already carry \
         the full metadata, so an override is ambiguous)"
    )]
    OverrideOnProjectLocal {
        /// The conflicting path.
        path: SecretPath,
    },
}

/// Merge the per-project manifest with the global index per ADR-020 §4.
///
/// Returns a [`MergeOutput`] holding one [`ResolvedSecret`] per declared
/// path plus any advisory warnings. Returns [`MergeError`] on hard
/// rule violations (`E_SECRET_UNKNOWN_PATH`, `OverrideOnProjectLocal`).
pub fn merge_manifest(
    global: &GlobalIndex,
    manifest: &ProjectManifest,
) -> Result<MergeOutput, MergeError> {
    // Hard error: a `[secret."x"]` and an `[overrides."x"]` for the
    // same `x` is ambiguous. Run this check up-front so the resolver
    // body below can assume the two namespaces are disjoint per path.
    for path in manifest.secrets.keys() {
        if manifest.overrides.contains_key(path) {
            return Err(MergeError::OverrideOnProjectLocal { path: path.clone() });
        }
    }

    let mut output = MergeOutput::default();

    // Resolve each declared path. We iterate `required` then `optional`
    // so the `required` flag on `ResolvedSecret` is set correctly even
    // for paths that appear in both lists (informally — the spec does
    // not forbid duplication; the merge upgrades to required).
    for (path, is_required) in manifest
        .required
        .iter()
        .map(|p| (p, true))
        .chain(manifest.optional.iter().map(|p| (p, false)))
    {
        let resolved = resolve_one(path, is_required, global, manifest, &mut output.warnings)?;
        // If a path appears in both lists, the second pass (optional)
        // would normally clobber the first. Preserve the stronger
        // requirement instead.
        match output.secrets.get(path) {
            Some(existing) if existing.required && !is_required => continue,
            _ => {
                output.secrets.insert(path.clone(), resolved);
            }
        }
    }

    // Advisory warnings for `[overrides]` and `[secret]` blocks that
    // don't correspond to any declared path.
    for path in manifest.overrides.keys() {
        if !is_declared(path, manifest) {
            output.warnings.push(MergeWarning {
                kind: MergeWarningKind::OverrideForUndeclaredPath,
                path: path.clone(),
            });
        }
    }
    for path in manifest.secrets.keys() {
        if !is_declared(path, manifest) {
            output.warnings.push(MergeWarning {
                kind: MergeWarningKind::ProjectLocalForUndeclaredPath,
                path: path.clone(),
            });
        }
    }

    Ok(output)
}

fn is_declared(path: &SecretPath, manifest: &ProjectManifest) -> bool {
    manifest.required.iter().any(|p| p == path) || manifest.optional.iter().any(|p| p == path)
}

fn resolve_one(
    path: &SecretPath,
    is_required: bool,
    global: &GlobalIndex,
    manifest: &ProjectManifest,
    warnings: &mut Vec<MergeWarning>,
) -> Result<ResolvedSecret, MergeError> {
    // Rule 1: project-local entry wins outright.
    if let Some(local) = manifest.secrets.get(path) {
        return Ok(ResolvedSecret {
            path: path.clone(),
            required: is_required,
            origin: SecretOrigin::ProjectLocal,
            metadata: local.clone(),
        });
    }

    // Rule 2: global entry, optionally overridden.
    if let Some(global_entry) = global.get(path) {
        let mut metadata = global_entry.clone();
        let mut applied = Vec::new();
        if let Some(over) = manifest.overrides.get(path) {
            apply_overrides(&mut metadata, over, path, &mut applied, warnings);
        }
        return Ok(ResolvedSecret {
            path: path.clone(),
            required: is_required,
            origin: SecretOrigin::Global {
                overrides_applied: applied,
            },
            metadata,
        });
    }

    // Rule 3: nowhere → E_SECRET_UNKNOWN_PATH.
    Err(MergeError::UnknownPath {
        path: path.clone(),
        role: if is_required {
            PathRole::Required
        } else {
            PathRole::Optional
        },
    })
}

fn apply_overrides(
    metadata: &mut IndexEntry,
    over: &OverrideEntry,
    path: &SecretPath,
    applied: &mut Vec<OverrideField>,
    warnings: &mut Vec<MergeWarning>,
) {
    if let Some(g) = over.gate {
        if metadata.default_gate == Some(g) {
            warnings.push(MergeWarning {
                kind: MergeWarningKind::NoOpOverride {
                    field: OverrideField::Gate,
                },
                path: path.clone(),
            });
        }
        metadata.default_gate = Some(g);
        applied.push(OverrideField::Gate);
    }
    if let Some(d) = over.rotate_every_days {
        if metadata.rotate_every_days == Some(d) {
            warnings.push(MergeWarning {
                kind: MergeWarningKind::NoOpOverride {
                    field: OverrideField::RotateEveryDays,
                },
                path: path.clone(),
            });
        }
        metadata.rotate_every_days = Some(d);
        applied.push(OverrideField::RotateEveryDays);
    }
    if let Some(desc) = &over.description {
        if metadata.description.as_ref() == Some(desc) {
            warnings.push(MergeWarning {
                kind: MergeWarningKind::NoOpOverride {
                    field: OverrideField::Description,
                },
                path: path.clone(),
            });
        }
        metadata.description = Some(desc.clone());
        applied.push(OverrideField::Description);
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::{Gate, IndexEntry, RotationMethod};

    fn p(s: &str) -> SecretPath {
        SecretPath::parse(s).unwrap()
    }

    // -- Happy paths -----------------------------------------------------------

    #[test]
    fn resolves_global_entry_without_overrides() {
        let mut global = GlobalIndex::new();
        global.insert(
            p("team/gitlab/token-deploy"),
            IndexEntry {
                description: Some("Team deploy token".to_owned()),
                default_gate: Some(Gate::Auto),
                rotation_method: Some(RotationMethod::Manual),
                ..IndexEntry::default()
            },
        );
        let manifest = ProjectManifest {
            required: vec![p("team/gitlab/token-deploy")],
            ..ProjectManifest::default()
        };

        let out = merge_manifest(&global, &manifest).unwrap();
        let resolved = out.secrets.get(&p("team/gitlab/token-deploy")).unwrap();
        assert!(resolved.required);
        assert_eq!(
            resolved.origin,
            SecretOrigin::Global {
                overrides_applied: vec![]
            }
        );
        assert_eq!(
            resolved.metadata.description.as_deref(),
            Some("Team deploy token")
        );
        assert!(out.warnings.is_empty());
    }

    #[test]
    fn applies_overrides_on_global_entry() {
        let mut global = GlobalIndex::new();
        global.insert(
            p("team/gitlab/token-deploy"),
            IndexEntry {
                description: Some("Team deploy token".to_owned()),
                default_gate: Some(Gate::Auto),
                rotate_every_days: Some(90),
                ..IndexEntry::default()
            },
        );
        let manifest = ProjectManifest {
            required: vec![p("team/gitlab/token-deploy")],
            overrides: BTreeMap::from([(
                p("team/gitlab/token-deploy"),
                OverrideEntry {
                    gate: Some(Gate::Touchid),
                    rotate_every_days: Some(30),
                    description: Some("Staging deploy only".to_owned()),
                },
            )]),
            ..ProjectManifest::default()
        };

        let out = merge_manifest(&global, &manifest).unwrap();
        let resolved = out.secrets.get(&p("team/gitlab/token-deploy")).unwrap();

        match &resolved.origin {
            SecretOrigin::Global { overrides_applied } => {
                assert_eq!(overrides_applied.len(), 3);
                assert!(overrides_applied.contains(&OverrideField::Gate));
                assert!(overrides_applied.contains(&OverrideField::RotateEveryDays));
                assert!(overrides_applied.contains(&OverrideField::Description));
            }
            other => panic!("expected Global origin, got {other:?}"),
        }
        assert_eq!(resolved.metadata.default_gate, Some(Gate::Touchid));
        assert_eq!(resolved.metadata.rotate_every_days, Some(30));
        assert_eq!(
            resolved.metadata.description.as_deref(),
            Some("Staging deploy only")
        );
        assert!(out.warnings.is_empty());
    }

    #[test]
    fn project_local_wins_over_absent_global() {
        let global = GlobalIndex::new();
        let manifest = ProjectManifest {
            required: vec![p("sandbox/example/token")],
            secrets: BTreeMap::from([(
                p("sandbox/example/token"),
                IndexEntry {
                    description: Some("Sandbox-only".to_owned()),
                    pattern_id: Some("generic-bearer".to_owned()),
                    ..IndexEntry::default()
                },
            )]),
            ..ProjectManifest::default()
        };

        let out = merge_manifest(&global, &manifest).unwrap();
        let r = out.secrets.get(&p("sandbox/example/token")).unwrap();
        assert_eq!(r.origin, SecretOrigin::ProjectLocal);
        assert_eq!(r.metadata.description.as_deref(), Some("Sandbox-only"));
    }

    #[test]
    fn project_local_wins_over_global_when_both_present() {
        // The spec is explicit: rule 1 (project-local) wins outright.
        let mut global = GlobalIndex::new();
        global.insert(
            p("team/foo/token"),
            IndexEntry {
                description: Some("Global description".to_owned()),
                ..IndexEntry::default()
            },
        );
        let manifest = ProjectManifest {
            required: vec![p("team/foo/token")],
            secrets: BTreeMap::from([(
                p("team/foo/token"),
                IndexEntry {
                    description: Some("Project-local description".to_owned()),
                    ..IndexEntry::default()
                },
            )]),
            ..ProjectManifest::default()
        };

        let out = merge_manifest(&global, &manifest).unwrap();
        let r = out.secrets.get(&p("team/foo/token")).unwrap();
        assert_eq!(r.origin, SecretOrigin::ProjectLocal);
        assert_eq!(
            r.metadata.description.as_deref(),
            Some("Project-local description")
        );
    }

    #[test]
    fn optional_path_resolved_with_required_false() {
        let mut global = GlobalIndex::new();
        global.insert(p("personal/slack/notify-token"), IndexEntry::default());
        let manifest = ProjectManifest {
            optional: vec![p("personal/slack/notify-token")],
            ..ProjectManifest::default()
        };

        let out = merge_manifest(&global, &manifest).unwrap();
        let r = out.secrets.get(&p("personal/slack/notify-token")).unwrap();
        assert!(!r.required);
    }

    #[test]
    fn path_in_both_required_and_optional_resolves_as_required() {
        let mut global = GlobalIndex::new();
        global.insert(p("team/foo/token"), IndexEntry::default());
        let manifest = ProjectManifest {
            required: vec![p("team/foo/token")],
            optional: vec![p("team/foo/token")],
            ..ProjectManifest::default()
        };

        let out = merge_manifest(&global, &manifest).unwrap();
        let r = out.secrets.get(&p("team/foo/token")).unwrap();
        assert!(
            r.required,
            "required must win when path appears in both lists"
        );
    }

    // -- Hard errors -----------------------------------------------------------

    #[test]
    fn unknown_required_path_errors() {
        let global = GlobalIndex::new();
        let manifest = ProjectManifest {
            required: vec![p("team/gitlab/token-deploy")],
            ..ProjectManifest::default()
        };

        let err = merge_manifest(&global, &manifest).unwrap_err();
        match err {
            MergeError::UnknownPath { path, role } => {
                assert_eq!(path.as_str(), "team/gitlab/token-deploy");
                assert_eq!(role, PathRole::Required);
            }
            other => panic!("expected UnknownPath, got {other:?}"),
        }
    }

    #[test]
    fn unknown_optional_path_errors_with_optional_role() {
        let global = GlobalIndex::new();
        let manifest = ProjectManifest {
            optional: vec![p("personal/slack/notify-token")],
            ..ProjectManifest::default()
        };

        let err = merge_manifest(&global, &manifest).unwrap_err();
        assert!(matches!(
            err,
            MergeError::UnknownPath {
                role: PathRole::Optional,
                ..
            }
        ));
    }

    #[test]
    fn override_on_project_local_path_errors() {
        let manifest = ProjectManifest {
            required: vec![p("sandbox/foo/token")],
            secrets: BTreeMap::from([(p("sandbox/foo/token"), IndexEntry::default())]),
            overrides: BTreeMap::from([(
                p("sandbox/foo/token"),
                OverrideEntry {
                    gate: Some(Gate::Touchid),
                    ..OverrideEntry::default()
                },
            )]),
            ..ProjectManifest::default()
        };

        let err = merge_manifest(&GlobalIndex::new(), &manifest).unwrap_err();
        match err {
            MergeError::OverrideOnProjectLocal { path } => {
                assert_eq!(path.as_str(), "sandbox/foo/token");
            }
            other => panic!("expected OverrideOnProjectLocal, got {other:?}"),
        }
    }

    // -- Warnings --------------------------------------------------------------

    #[test]
    fn no_op_override_emits_warning_per_field() {
        let mut global = GlobalIndex::new();
        global.insert(
            p("team/foo/token"),
            IndexEntry {
                default_gate: Some(Gate::Touchid),
                rotate_every_days: Some(30),
                description: Some("matches".to_owned()),
                ..IndexEntry::default()
            },
        );
        let manifest = ProjectManifest {
            required: vec![p("team/foo/token")],
            overrides: BTreeMap::from([(
                p("team/foo/token"),
                OverrideEntry {
                    gate: Some(Gate::Touchid),
                    rotate_every_days: Some(30),
                    description: Some("matches".to_owned()),
                },
            )]),
            ..ProjectManifest::default()
        };

        let out = merge_manifest(&global, &manifest).unwrap();
        // Three warnings — one per redundant field.
        assert_eq!(out.warnings.len(), 3);
        let kinds: Vec<&MergeWarningKind> = out.warnings.iter().map(|w| &w.kind).collect();
        assert!(kinds.iter().any(|k| matches!(
            k,
            MergeWarningKind::NoOpOverride {
                field: OverrideField::Gate
            }
        )));
        assert!(kinds.iter().any(|k| matches!(
            k,
            MergeWarningKind::NoOpOverride {
                field: OverrideField::RotateEveryDays
            }
        )));
        assert!(kinds.iter().any(|k| matches!(
            k,
            MergeWarningKind::NoOpOverride {
                field: OverrideField::Description
            }
        )));
    }

    #[test]
    fn override_for_undeclared_path_emits_warning() {
        // Global has the path; manifest declares NOTHING but has an
        // override for the path. Override is unused — warn.
        let mut global = GlobalIndex::new();
        global.insert(p("team/foo/token"), IndexEntry::default());
        let manifest = ProjectManifest {
            overrides: BTreeMap::from([(
                p("team/foo/token"),
                OverrideEntry {
                    gate: Some(Gate::Touchid),
                    ..OverrideEntry::default()
                },
            )]),
            ..ProjectManifest::default()
        };

        let out = merge_manifest(&global, &manifest).unwrap();
        assert!(out.secrets.is_empty());
        assert_eq!(out.warnings.len(), 1);
        assert!(matches!(
            out.warnings[0].kind,
            MergeWarningKind::OverrideForUndeclaredPath
        ));
        assert_eq!(out.warnings[0].path.as_str(), "team/foo/token");
    }

    #[test]
    fn project_local_for_undeclared_path_emits_warning() {
        let manifest = ProjectManifest {
            secrets: BTreeMap::from([(
                p("sandbox/orphan/token"),
                IndexEntry {
                    description: Some("orphan".to_owned()),
                    ..IndexEntry::default()
                },
            )]),
            ..ProjectManifest::default()
        };

        let out = merge_manifest(&GlobalIndex::new(), &manifest).unwrap();
        assert!(out.secrets.is_empty());
        assert_eq!(out.warnings.len(), 1);
        assert!(matches!(
            out.warnings[0].kind,
            MergeWarningKind::ProjectLocalForUndeclaredPath
        ));
    }

    // -- Empty cases -----------------------------------------------------------

    #[test]
    fn empty_manifest_yields_empty_output() {
        let global = GlobalIndex::new();
        let manifest = ProjectManifest::new();
        let out = merge_manifest(&global, &manifest).unwrap();
        assert!(out.secrets.is_empty());
        assert!(out.warnings.is_empty());
    }

    #[test]
    fn empty_global_with_only_project_local_required_works() {
        let manifest = ProjectManifest {
            required: vec![p("sandbox/example/token")],
            secrets: BTreeMap::from([(
                p("sandbox/example/token"),
                IndexEntry {
                    description: Some("local".to_owned()),
                    ..IndexEntry::default()
                },
            )]),
            ..ProjectManifest::default()
        };

        let out = merge_manifest(&GlobalIndex::new(), &manifest).unwrap();
        assert_eq!(out.secrets.len(), 1);
        assert_eq!(
            out.secrets.get(&p("sandbox/example/token")).unwrap().origin,
            SecretOrigin::ProjectLocal
        );
    }

    // -- Sorted iteration of output -------------------------------------------

    #[test]
    fn output_secrets_iter_sorted_by_path() {
        let mut global = GlobalIndex::new();
        global.insert(p("team/zoo/key"), IndexEntry::default());
        global.insert(p("personal/foo/key"), IndexEntry::default());
        global.insert(p("client-acme/bar/key"), IndexEntry::default());

        let manifest = ProjectManifest {
            required: vec![
                p("team/zoo/key"),
                p("personal/foo/key"),
                p("client-acme/bar/key"),
            ],
            ..ProjectManifest::default()
        };

        let out = merge_manifest(&global, &manifest).unwrap();
        let paths: Vec<&str> = out.secrets.keys().map(|p| p.as_str()).collect();
        assert_eq!(
            paths,
            vec!["client-acme/bar/key", "personal/foo/key", "team/zoo/key"]
        );
    }
}
