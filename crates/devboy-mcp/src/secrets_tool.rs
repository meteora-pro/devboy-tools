//! `secrets_list` and `secrets_describe` MCP tool helpers per
//! [ADR-023] §3.7 (agent protocol — read-only metadata access).
//!
//! Pure-data builders that take a [`GlobalIndex`] and a
//! [`ProjectManifest`], merge them, and emit JSON-shaped views
//! the MCP server can serialise straight back to the client.
//!
//! ## Manifest gating
//!
//! Per ADR-023 §3.7: "the agent only sees secrets the active
//! context's manifest declares — global-index entries that no
//! merged manifest references are invisible." We honour this
//! by basing both views on `MergeOutput::secrets`, which is
//! already manifest-gated.
//!
//! ## What is **never** returned
//!
//! Neither `list` nor `describe` ever round-trips through a
//! source — no values, no live token strings, no probe results
//! that would leak provider-side secrets. The replies carry
//! metadata only: paths, ADR-020 fields, computed status,
//! capability hints, and a source hint. ADR-023 §3.7's
//! "agent never sees the value" rule is enforced **by absence
//! of a `value` field**, not by a runtime guard — this module
//! has no knob to turn off.
//!
//! [ADR-023]: https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-023-secret-store-ux-layer.md

use chrono::{Local, NaiveDate};
use devboy_storage::{
    GlobalIndex, IndexEntry, MergeOutput, ProjectManifest, ResolvedSecret, RotationMethod,
    SecretPath, merge_manifest,
};
use serde::{Deserialize, Serialize};

// =============================================================================
// Tool inputs
// =============================================================================

/// Optional filter passed to `secrets_list`. All fields are
/// AND-composed; a `None` field passes every value through.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SecretsListFilter {
    /// Substring match against the full ADR-020 path. Case
    /// sensitive (paths are kebab-case lower).
    pub path_contains: Option<String>,
    /// Exact match against the first path segment (the scope).
    pub scope: Option<String>,
    /// Status filter — see [`SecretsStatus`] for the values.
    pub status: Option<SecretsStatus>,
    /// Include framework-internal paths (`__*`). Hidden by
    /// default, matching `devboy secrets list` (ADR-021 §5).
    pub include_internal: bool,
}

// =============================================================================
// Tool outputs
// =============================================================================

/// Computed lifecycle status. Derived from `expires_at`
/// alone — actual provisioning state would require a router
/// probe, which the agent surface deliberately does not perform
/// (probes might leak that the user has revoked a token, etc.).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SecretsStatus {
    /// Entry is registered and `expires_at` is either unset or
    /// further than 14 days out.
    Registered,
    /// `expires_at` falls within the next 14 days.
    Expiring,
    /// `expires_at` is in the past — caller should rotate.
    Expired,
}

impl SecretsStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Registered => "registered",
            Self::Expiring => "expiring",
            Self::Expired => "expired",
        }
    }
}

/// One row in the `secrets_list` reply.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SecretsListItem {
    pub path: String,
    pub status: SecretsStatus,
    pub expires_at: Option<String>,
    /// Hint about the routed source. Currently always `None`
    /// because the MCP server does not yet hold a router
    /// instance — see the module preamble. Reserved as a stable
    /// field on the JSON contract.
    pub source_name: Option<String>,
    /// Comma-separated capability hints derived from
    /// [`IndexEntry::rotation_method`]: always includes `read`,
    /// adds `rotate` when the rotation method is non-manual.
    pub capabilities_hint: String,
}

impl crate::agent_safety::AgentSafeReply for SecretsListItem {}

/// Detail view returned by `secrets_describe`. Strict
/// superset of [`SecretsListItem`] — the same row plus the
/// metadata fields a user would want to inspect.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SecretsDescribeReply {
    pub path: String,
    pub status: SecretsStatus,
    pub expires_at: Option<String>,
    pub source_name: Option<String>,
    pub capabilities_hint: String,
    pub description: Option<String>,
    pub retrieval_url: Option<String>,
    pub rotation_method: Option<String>,
    pub last_rotated_at: Option<String>,
    pub rotate_every_days: Option<u32>,
    pub pattern_id: Option<String>,
}

impl crate::agent_safety::AgentSafeReply for SecretsDescribeReply {}

/// Error variants surfaced as MCP tool errors.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum SecretsToolError {
    /// `path` argument is not a valid ADR-020 path.
    InvalidPath { path: String, reason: String },
    /// Path requested by `describe` isn't visible in the active
    /// context's merged manifest.
    NotFound { path: String },
    /// Merging the global index with the project manifest
    /// surfaced a hard error (e.g. duplicate entry conflict
    /// per ADR-020 §6).
    MergeFailed { reason: String },
}

// =============================================================================
// Pure helpers
// =============================================================================

/// Build the `secrets_list` reply. Pure over inputs — caller
/// loads the index + manifest from wherever (defaults, CLI
/// override, fixture).
pub fn list(
    index: &GlobalIndex,
    manifest: &ProjectManifest,
    filter: &SecretsListFilter,
    today: NaiveDate,
) -> Result<Vec<SecretsListItem>, SecretsToolError> {
    let merged = build_merged(index, manifest)?;
    let mut rows: Vec<SecretsListItem> = merged
        .secrets
        .values()
        .filter(|r| filter_matches(r, filter, today))
        .map(|r| build_list_item(r, today))
        .collect();
    rows.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(rows)
}

/// Build the `secrets_describe` reply for `path`.
pub fn describe(
    index: &GlobalIndex,
    manifest: &ProjectManifest,
    path: &str,
    today: NaiveDate,
) -> Result<SecretsDescribeReply, SecretsToolError> {
    let parsed = SecretPath::parse(path).map_err(|e| SecretsToolError::InvalidPath {
        path: path.to_owned(),
        reason: e.to_string(),
    })?;
    let merged = build_merged(index, manifest)?;
    let row = merged
        .secrets
        .get(&parsed)
        .ok_or_else(|| SecretsToolError::NotFound {
            path: path.to_owned(),
        })?;
    Ok(build_describe(row, today))
}

fn build_merged(
    index: &GlobalIndex,
    manifest: &ProjectManifest,
) -> Result<MergeOutput, SecretsToolError> {
    merge_manifest(index, manifest).map_err(|e| SecretsToolError::MergeFailed {
        reason: e.to_string(),
    })
}

fn filter_matches(row: &ResolvedSecret, filter: &SecretsListFilter, today: NaiveDate) -> bool {
    let path_str = row.path.as_str();
    if !filter.include_internal && row.path.scope().starts_with("__") {
        return false;
    }
    if let Some(needle) = filter.path_contains.as_deref()
        && !path_str.contains(needle)
    {
        return false;
    }
    if let Some(scope) = filter.scope.as_deref() {
        let first = path_str.split('/').next().unwrap_or("");
        if first != scope {
            return false;
        }
    }
    if let Some(want) = filter.status
        && compute_status(&row.metadata, today) != want
    {
        return false;
    }
    true
}

fn build_list_item(row: &ResolvedSecret, today: NaiveDate) -> SecretsListItem {
    SecretsListItem {
        path: row.path.to_string(),
        status: compute_status(&row.metadata, today),
        expires_at: row.metadata.expires_at.clone(),
        source_name: None,
        capabilities_hint: capabilities_hint(&row.metadata),
    }
}

fn build_describe(row: &ResolvedSecret, today: NaiveDate) -> SecretsDescribeReply {
    SecretsDescribeReply {
        path: row.path.to_string(),
        status: compute_status(&row.metadata, today),
        expires_at: row.metadata.expires_at.clone(),
        source_name: None,
        capabilities_hint: capabilities_hint(&row.metadata),
        description: row.metadata.description.clone(),
        retrieval_url: row.metadata.retrieval_url.clone(),
        rotation_method: row
            .metadata
            .rotation_method
            .map(|m| rotation_label(m).to_string()),
        last_rotated_at: row.metadata.last_rotated_at.clone(),
        rotate_every_days: row.metadata.rotate_every_days,
        pattern_id: row.metadata.pattern_id.clone(),
    }
}

fn compute_status(entry: &IndexEntry, today: NaiveDate) -> SecretsStatus {
    let Some(raw) = entry.expires_at.as_deref() else {
        return SecretsStatus::Registered;
    };
    let Ok(date) = NaiveDate::parse_from_str(raw, "%Y-%m-%d") else {
        return SecretsStatus::Registered;
    };
    let days_until = (date - today).num_days();
    if days_until < 0 {
        SecretsStatus::Expired
    } else if days_until <= 14 {
        SecretsStatus::Expiring
    } else {
        SecretsStatus::Registered
    }
}

fn capabilities_hint(entry: &IndexEntry) -> String {
    match entry.rotation_method.unwrap_or_default() {
        RotationMethod::Manual => "read".to_string(),
        RotationMethod::ProviderUi | RotationMethod::ProviderApi => "read,rotate".to_string(),
    }
}

fn rotation_label(m: RotationMethod) -> &'static str {
    match m {
        RotationMethod::Manual => "manual",
        RotationMethod::ProviderUi => "provider-ui",
        RotationMethod::ProviderApi => "provider-api",
    }
}

/// Convenience: today in the local timezone, formatted as
/// `NaiveDate`. Pulled out so tests can pass a fixed date.
pub fn today_local() -> NaiveDate {
    Local::now().date_naive()
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    fn today() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 5, 9).unwrap()
    }

    fn entry_with_expiry(expiry: Option<&str>) -> IndexEntry {
        IndexEntry {
            description: Some("Jira API token".into()),
            retrieval_url: Some("https://example.invalid/jira".into()),
            expires_at: expiry.map(str::to_owned),
            rotation_method: Some(RotationMethod::Manual),
            pattern_id: Some("jira-api-token".into()),
            ..IndexEntry::default()
        }
    }

    fn index_with(path: &str, entry: IndexEntry) -> GlobalIndex {
        let body = format!(
            "[secret.\"{path}\"]\ndescription = {:?}\nretrieval_url = {:?}\n{}{}{}{}\n",
            entry.description.as_deref().unwrap_or(""),
            entry.retrieval_url.as_deref().unwrap_or(""),
            entry
                .expires_at
                .as_deref()
                .map(|d| format!("expires_at = \"{d}\"\n"))
                .unwrap_or_default(),
            entry
                .rotation_method
                .map(|m| format!(
                    "rotation_method = \"{}\"\n",
                    match m {
                        RotationMethod::Manual => "manual",
                        RotationMethod::ProviderUi => "provider-ui",
                        RotationMethod::ProviderApi => "provider-api",
                    }
                ))
                .unwrap_or_default(),
            entry
                .pattern_id
                .as_deref()
                .map(|p| format!("pattern_id = \"{p}\"\n"))
                .unwrap_or_default(),
            entry
                .last_rotated_at
                .as_deref()
                .map(|d| format!("last_rotated_at = \"{d}\"\n"))
                .unwrap_or_default(),
        );
        GlobalIndex::from_toml_str(&body).unwrap()
    }

    fn manifest_with(path: &str) -> ProjectManifest {
        let body = format!("required = [\"{path}\"]\n");
        ProjectManifest::from_toml_str(&body).unwrap()
    }

    // -- Status computation -----------------------------------

    #[test]
    fn status_registered_when_no_expiry() {
        assert_eq!(
            compute_status(&entry_with_expiry(None), today()),
            SecretsStatus::Registered
        );
    }

    #[test]
    fn status_registered_when_expiry_far_in_future() {
        assert_eq!(
            compute_status(&entry_with_expiry(Some("2027-01-01")), today()),
            SecretsStatus::Registered
        );
    }

    #[test]
    fn status_expiring_within_14_days() {
        // today = 2026-05-09; +10d = 2026-05-19
        assert_eq!(
            compute_status(&entry_with_expiry(Some("2026-05-19")), today()),
            SecretsStatus::Expiring
        );
    }

    #[test]
    fn status_expired_when_in_the_past() {
        assert_eq!(
            compute_status(&entry_with_expiry(Some("2026-01-01")), today()),
            SecretsStatus::Expired
        );
    }

    #[test]
    fn malformed_expiry_string_falls_back_to_registered() {
        assert_eq!(
            compute_status(&entry_with_expiry(Some("tomorrow")), today()),
            SecretsStatus::Registered
        );
    }

    // -- Capabilities hint ------------------------------------

    #[test]
    fn capabilities_hint_for_manual_rotation_is_read_only() {
        let mut e = entry_with_expiry(None);
        e.rotation_method = Some(RotationMethod::Manual);
        assert_eq!(capabilities_hint(&e), "read");
    }

    #[test]
    fn capabilities_hint_for_provider_ui_includes_rotate() {
        let mut e = entry_with_expiry(None);
        e.rotation_method = Some(RotationMethod::ProviderUi);
        assert_eq!(capabilities_hint(&e), "read,rotate");
    }

    #[test]
    fn capabilities_hint_for_provider_api_includes_rotate() {
        let mut e = entry_with_expiry(None);
        e.rotation_method = Some(RotationMethod::ProviderApi);
        assert_eq!(capabilities_hint(&e), "read,rotate");
    }

    // -- list() ------------------------------------------------

    #[test]
    fn list_returns_only_paths_in_active_manifest() {
        // Index has two entries, manifest only requires one.
        let mut idx = GlobalIndex::from_toml_str("").unwrap();
        for path in ["team/jira/api-key", "team/github/api-key"] {
            let body = format!("[secret.\"{path}\"]\nrotation_method = \"manual\"\n");
            let parsed = GlobalIndex::from_toml_str(&body).unwrap();
            for (k, v) in parsed.iter() {
                idx.insert(k.clone(), v.clone());
            }
        }
        let manifest = manifest_with("team/jira/api-key");
        let rows = list(&idx, &manifest, &SecretsListFilter::default(), today()).unwrap();
        let paths: Vec<&str> = rows.iter().map(|r| r.path.as_str()).collect();
        assert_eq!(paths, vec!["team/jira/api-key"]);
    }

    #[test]
    fn list_status_filter_keeps_only_matching_rows() {
        let idx = index_with("team/jira/api-key", entry_with_expiry(Some("2026-01-01")));
        let manifest = manifest_with("team/jira/api-key");
        let filter = SecretsListFilter {
            status: Some(SecretsStatus::Expired),
            ..SecretsListFilter::default()
        };
        let rows = list(&idx, &manifest, &filter, today()).unwrap();
        assert_eq!(rows.len(), 1);
        let filter2 = SecretsListFilter {
            status: Some(SecretsStatus::Registered),
            ..SecretsListFilter::default()
        };
        assert!(list(&idx, &manifest, &filter2, today()).unwrap().is_empty());
    }

    #[test]
    fn list_path_contains_filter_substring_matches() {
        let idx = index_with("team/jira/api-key", entry_with_expiry(None));
        let manifest = manifest_with("team/jira/api-key");
        let f = SecretsListFilter {
            path_contains: Some("jira".into()),
            ..SecretsListFilter::default()
        };
        assert_eq!(list(&idx, &manifest, &f, today()).unwrap().len(), 1);
        let f2 = SecretsListFilter {
            path_contains: Some("github".into()),
            ..SecretsListFilter::default()
        };
        assert!(list(&idx, &manifest, &f2, today()).unwrap().is_empty());
    }

    #[test]
    fn list_scope_filter_matches_first_path_segment_only() {
        let idx = index_with("team/jira/api-key", entry_with_expiry(None));
        let manifest = manifest_with("team/jira/api-key");
        let f = SecretsListFilter {
            scope: Some("personal".into()),
            ..SecretsListFilter::default()
        };
        assert!(list(&idx, &manifest, &f, today()).unwrap().is_empty());
        let f2 = SecretsListFilter {
            scope: Some("team".into()),
            ..SecretsListFilter::default()
        };
        assert_eq!(list(&idx, &manifest, &f2, today()).unwrap().len(), 1);
    }

    #[test]
    fn list_response_does_not_carry_value_field() {
        // Compile-time fence — SecretsListItem has no value field.
        let idx = index_with("team/jira/api-key", entry_with_expiry(None));
        let manifest = manifest_with("team/jira/api-key");
        let rows = list(&idx, &manifest, &SecretsListFilter::default(), today()).unwrap();
        let json = serde_json::to_string(&rows[0]).unwrap();
        assert!(!json.contains("\"value\""));
    }

    #[test]
    fn list_rows_are_sorted_by_path_for_stable_output() {
        let mut idx = GlobalIndex::from_toml_str("").unwrap();
        let mut required_paths = String::from("required = [\n");
        for path in [
            "team/zulip/api-key",
            "team/jira/api-key",
            "team/github/api-key",
        ] {
            let body = format!("[secret.\"{path}\"]\nrotation_method = \"manual\"\n");
            let parsed = GlobalIndex::from_toml_str(&body).unwrap();
            for (k, v) in parsed.iter() {
                idx.insert(k.clone(), v.clone());
            }
            required_paths.push_str(&format!("    \"{path}\",\n"));
        }
        required_paths.push_str("]\n");
        let manifest = ProjectManifest::from_toml_str(&required_paths).unwrap();
        let rows = list(&idx, &manifest, &SecretsListFilter::default(), today()).unwrap();
        let paths: Vec<&str> = rows.iter().map(|r| r.path.as_str()).collect();
        assert_eq!(
            paths,
            vec![
                "team/github/api-key",
                "team/jira/api-key",
                "team/zulip/api-key",
            ]
        );
    }

    // -- describe() --------------------------------------------

    #[test]
    fn describe_returns_full_metadata_for_known_path() {
        let idx = index_with("team/jira/api-key", entry_with_expiry(Some("2026-12-01")));
        let manifest = manifest_with("team/jira/api-key");
        let reply = describe(&idx, &manifest, "team/jira/api-key", today()).unwrap();
        assert_eq!(reply.path, "team/jira/api-key");
        assert_eq!(reply.status, SecretsStatus::Registered);
        assert_eq!(reply.expires_at.as_deref(), Some("2026-12-01"));
        assert_eq!(reply.description.as_deref(), Some("Jira API token"));
        assert_eq!(
            reply.retrieval_url.as_deref(),
            Some("https://example.invalid/jira")
        );
        assert_eq!(reply.rotation_method.as_deref(), Some("manual"));
        assert_eq!(reply.pattern_id.as_deref(), Some("jira-api-token"));
        assert_eq!(reply.capabilities_hint, "read");
    }

    #[test]
    fn describe_errors_with_invalid_path_kind_for_bad_input() {
        let idx = GlobalIndex::from_toml_str("").unwrap();
        let manifest = ProjectManifest::from_toml_str("").unwrap();
        let err = describe(&idx, &manifest, "not a path", today()).unwrap_err();
        assert!(matches!(err, SecretsToolError::InvalidPath { .. }));
    }

    #[test]
    fn describe_errors_with_not_found_when_path_outside_active_manifest() {
        let idx = index_with("team/jira/api-key", entry_with_expiry(None));
        let manifest = ProjectManifest::from_toml_str("").unwrap();
        let err = describe(&idx, &manifest, "team/jira/api-key", today()).unwrap_err();
        assert!(matches!(err, SecretsToolError::NotFound { .. }));
    }

    #[test]
    fn describe_response_does_not_carry_value_field() {
        let idx = index_with("team/jira/api-key", entry_with_expiry(None));
        let manifest = manifest_with("team/jira/api-key");
        let reply = describe(&idx, &manifest, "team/jira/api-key", today()).unwrap();
        let json = serde_json::to_string(&reply).unwrap();
        assert!(!json.contains("\"value\""));
    }

    // -- internal-path filter ---------------------------------

    // Internal-prefix paths (`__sys/...`) cannot reach a user-facing
    // manifest per ADR-021 §5 — the manifest parser rejects them. The
    // `include_internal` filter still exists on the public surface so
    // an in-process caller (e.g. a future `secrets list --internal`
    // bridge) can opt in. These two tests exercise `filter_matches`
    // directly with a synthetic `ResolvedSecret`.
    use devboy_storage::SecretOrigin;

    fn synth_row(path: &str, internal: bool) -> ResolvedSecret {
        let parsed = if internal {
            SecretPath::parse_internal(path).unwrap()
        } else {
            SecretPath::parse(path).unwrap()
        };
        ResolvedSecret {
            path: parsed,
            required: true,
            origin: SecretOrigin::Global {
                overrides_applied: Vec::new(),
            },
            metadata: IndexEntry::default(),
        }
    }

    #[test]
    fn filter_hides_internal_paths_by_default() {
        let row = synth_row("__sys/router/seed", true);
        assert!(!filter_matches(
            &row,
            &SecretsListFilter::default(),
            today()
        ));
    }

    #[test]
    fn filter_admits_internal_paths_when_include_internal_set() {
        let row = synth_row("__sys/router/seed", true);
        let f = SecretsListFilter {
            include_internal: true,
            ..SecretsListFilter::default()
        };
        assert!(filter_matches(&row, &f, today()));
    }

    #[test]
    fn filter_admits_normal_paths_regardless_of_include_internal_flag() {
        let row = synth_row("team/jira/api-key", false);
        assert!(filter_matches(&row, &SecretsListFilter::default(), today()));
    }
}
