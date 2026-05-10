//! Jira provider metadata types for dynamic schema enrichment.

use serde::{Deserialize, Serialize};

/// Metadata for Jira project(s), used for dynamic schema enrichment.
///
/// Supports both single-project and multi-project configurations.
/// Multi-project unions enum values across projects.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JiraMetadata {
    /// Jira flavor (affects API version and auth).
    #[serde(default = "default_flavor")]
    pub flavor: JiraFlavor,
    /// Per-project metadata keyed by project key (e.g., "PROJ").
    pub projects: std::collections::HashMap<String, JiraProjectMetadata>,
    /// Structures the integration user can see across the Jira instance.
    ///
    /// `/rest/structure/2.0/structure` is **not** keyed by Jira project — it
    /// returns every structure the caller has read access to. Placed here
    /// (on the instance-level metadata) rather than on `JiraProjectMetadata`.
    /// Empty when the Structure plugin is not installed or the user has no
    /// read access; that is the graceful-degrade signal the schema enricher
    /// keys on to decide whether to enrich Structure tools.
    #[serde(default)]
    pub structures: Vec<JiraStructureRef>,
}

fn default_flavor() -> JiraFlavor {
    JiraFlavor::Cloud
}

/// Jira deployment flavor.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum JiraFlavor {
    /// Jira Cloud (API v3, ADF format, accountId-based users)
    Cloud,
    /// Jira Self-Hosted / Data Center (API v2, plain text, username-based users)
    SelfHosted,
}

/// Metadata for a single Jira project.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JiraProjectMetadata {
    /// Available issue types (filter out subtask types for create_issue).
    #[serde(default)]
    pub issue_types: Vec<JiraIssueType>,
    #[serde(default)]
    pub components: Vec<JiraComponent>,
    #[serde(default)]
    pub priorities: Vec<JiraPriority>,
    #[serde(default)]
    pub link_types: Vec<JiraLinkType>,
    #[serde(default)]
    pub custom_fields: Vec<JiraCustomField>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JiraIssueType {
    pub id: String,
    pub name: String,
    /// Whether this is a subtask type (exclude from create_issue enum).
    #[serde(default)]
    pub subtask: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JiraComponent {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JiraPriority {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JiraLinkType {
    pub id: String,
    pub name: String,
    /// Outward description (e.g., "blocks").
    #[serde(default)]
    pub outward: Option<String>,
    /// Inward description (e.g., "is blocked by").
    #[serde(default)]
    pub inward: Option<String>,
}

/// Jira custom field definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JiraCustomField {
    /// Field ID in Jira (e.g., "customfield_10001").
    pub id: String,
    /// Human-readable name.
    pub name: String,
    pub field_type: JiraFieldType,
    /// Whether this field is required.
    #[serde(default)]
    pub required: bool,
    /// Options for option/array fields.
    #[serde(default)]
    pub options: Vec<JiraFieldOption>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum JiraFieldType {
    /// Single select → name → `{ id: option_id }`.
    Option,
    /// Multi-select → name array → `[{ id }, ...]`.
    Array,
    /// Numeric → pass-through.
    Number,
    /// Date (YYYY-MM-DD) → pass-through.
    Date,
    /// DateTime (ISO 8601) → pass-through.
    DateTime,
    /// Free text → pass-through.
    String,
    /// Catch-all (epic link, etc.) → pass-through as string key.
    Any,
}

/// Option for Jira option/array custom fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JiraFieldOption {
    pub id: String,
    pub name: String,
}

/// Reference to a Jira Structure the integration user can access.
///
/// Populated from `/rest/structure/2.0/structure`. Stored in
/// [`JiraMetadata::structures`] and consumed by `JiraSchemaEnricher` to
/// add description-based hints for the `structureId` parameter on the 7
/// Structure tools that take it (the strict JSON Schema `enum` is deferred
/// until `PropertySchema.enum_values` supports non-string variants).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct JiraStructureRef {
    pub id: u64,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl JiraCustomField {
    /// Convert a human-readable value to Jira API format.
    ///
    /// - Option: name → `{ "id": "option_id" }`
    /// - Array: name array → `[{ "id": "id1" }, { "id": "id2" }]`
    /// - Other types: pass-through
    pub fn transform_value(&self, value: &serde_json::Value) -> serde_json::Value {
        match self.field_type {
            JiraFieldType::Option => {
                if let Some(name) = value.as_str()
                    && let Some(opt) = self
                        .options
                        .iter()
                        .find(|o| o.name.eq_ignore_ascii_case(name))
                {
                    return serde_json::json!({ "id": opt.id });
                }
                value.clone()
            }
            JiraFieldType::Array => {
                if let Some(names) = value.as_array() {
                    let ids: Vec<serde_json::Value> = names
                        .iter()
                        .filter_map(|n| {
                            let name = n.as_str()?;
                            self.options
                                .iter()
                                .find(|o| o.name.eq_ignore_ascii_case(name))
                                .map(|o| serde_json::json!({ "id": o.id }))
                        })
                        .collect();
                    return serde_json::json!(ids);
                }
                value.clone()
            }
            _ => value.clone(),
        }
    }
}

impl JiraMetadata {
    /// Whether this is a single-project configuration.
    pub fn is_single_project(&self) -> bool {
        self.projects.len() == 1
    }

    /// Get project keys.
    pub fn project_keys(&self) -> Vec<&str> {
        self.projects.keys().map(|k| k.as_str()).collect()
    }

    /// Get union of all issue types across projects (non-subtask only).
    pub fn all_issue_types(&self) -> Vec<String> {
        let mut types: Vec<String> = self
            .projects
            .values()
            .flat_map(|p| {
                p.issue_types
                    .iter()
                    .filter(|t| !t.subtask)
                    .map(|t| t.name.clone())
            })
            .collect();
        types.sort();
        types.dedup();
        types
    }

    /// Get union of all priorities across projects.
    pub fn all_priorities(&self) -> Vec<String> {
        let mut prios: Vec<String> = self
            .projects
            .values()
            .flat_map(|p| p.priorities.iter().map(|pr| pr.name.clone()))
            .collect();
        prios.sort();
        prios.dedup();
        prios
    }

    /// Get union of all components across projects.
    pub fn all_components(&self) -> Vec<String> {
        let mut comps: Vec<String> = self
            .projects
            .values()
            .flat_map(|p| p.components.iter().map(|c| c.name.clone()))
            .collect();
        comps.sort();
        comps.dedup();
        comps
    }

    /// Get union of all link types across projects.
    pub fn all_link_types(&self) -> Vec<String> {
        let mut types: Vec<String> = self
            .projects
            .values()
            .flat_map(|p| p.link_types.iter().map(|lt| lt.name.clone()))
            .collect();
        types.sort();
        types.dedup();
        types
    }

    /// Union of customfields visible across the first
    /// [`MAX_ENRICHMENT_PROJECTS`] projects, deduplicated by `name`.
    /// First-write-wins on collisions — if `Severity` exists in
    /// multiple projects with different ids, the schema is built
    /// from the earliest project's definition (matches HashMap
    /// iteration order; not deterministic across runs but stable
    /// within one). Per-project resolution at dispatch time is done
    /// by [`Self::custom_field_for_project`], which reads the
    /// current project's metadata directly.
    ///
    /// The cap protects token budgets on enterprise instances with
    /// hundreds of projects: enrichment beyond 30 projects emits a
    /// `tracing::warn!` and silently truncates rather than letting
    /// the schema explode. Selecting the relevant 30 (most-recent
    /// activity, allowlist, etc.) is the metadata loader's job, not
    /// this crate's.
    pub fn all_custom_fields(&self) -> Vec<JiraCustomField> {
        if self.projects.len() > MAX_ENRICHMENT_PROJECTS {
            tracing::warn!(
                project_count = self.projects.len(),
                cap = MAX_ENRICHMENT_PROJECTS,
                "Jira metadata carries more projects than the enrichment cap; \
                 customfield schema will only reflect the first {} (by sorted \
                 project key) — narrow the metadata loader's project \
                 selection (top-N by recency, allowlist, etc.) for full \
                 coverage.",
                MAX_ENRICHMENT_PROJECTS
            );
        }
        // Sort project keys before truncating so the "first 30" set
        // is deterministic across reloads — HashMap iteration order
        // is not stable (Codex review on PR #260).
        let mut project_keys: Vec<&String> = self.projects.keys().collect();
        project_keys.sort();
        let mut by_name: std::collections::HashMap<String, JiraCustomField> =
            std::collections::HashMap::new();
        for key in project_keys.iter().take(MAX_ENRICHMENT_PROJECTS) {
            if let Some(proj) = self.projects.get(*key) {
                for cf in &proj.custom_fields {
                    by_name.entry(cf.name.clone()).or_insert_with(|| cf.clone());
                }
            }
        }
        let mut result: Vec<JiraCustomField> = by_name.into_values().collect();
        result.sort_by(|a, b| a.name.cmp(&b.name));
        result
    }

    /// Resolve a customfield by display name **within a specific
    /// project** — used by `transform_args` to pick the right
    /// `customfield_*` id when the same name maps to different ids
    /// across projects in multi-project mode.
    pub fn custom_field_for_project(
        &self,
        project_key: &str,
        field_name: &str,
    ) -> Option<&JiraCustomField> {
        self.projects
            .get(project_key)?
            .custom_fields
            .iter()
            .find(|cf| cf.name == field_name)
    }

    /// Group customfields across capped projects by display name —
    /// returns `(name, [variants])` pairs sorted by name. A name
    /// with two entries that have different `field_type`s flags a
    /// cross-project shape conflict the enricher resolves with
    /// `anyOf` instead of first-wins.
    pub fn custom_field_groups(&self) -> Vec<(String, Vec<JiraCustomField>)> {
        if self.projects.len() > MAX_ENRICHMENT_PROJECTS {
            tracing::warn!(
                project_count = self.projects.len(),
                cap = MAX_ENRICHMENT_PROJECTS,
                "Jira metadata carries more projects than the enrichment cap; \
                 customfield groups will only reflect the first {} (by sorted \
                 project key).",
                MAX_ENRICHMENT_PROJECTS
            );
        }
        // Sort project keys before truncating so the selected
        // subset is deterministic across reloads — see
        // `all_custom_fields` (Codex review on PR #260).
        let mut project_keys: Vec<&String> = self.projects.keys().collect();
        project_keys.sort();
        let mut groups: std::collections::HashMap<String, Vec<JiraCustomField>> =
            std::collections::HashMap::new();
        for key in project_keys.iter().take(MAX_ENRICHMENT_PROJECTS) {
            if let Some(proj) = self.projects.get(*key) {
                for cf in &proj.custom_fields {
                    groups.entry(cf.name.clone()).or_default().push(cf.clone());
                }
            }
        }
        let mut result: Vec<(String, Vec<JiraCustomField>)> = groups.into_iter().collect();
        result.sort_by(|a, b| a.0.cmp(&b.0));
        result
    }
}

/// Cap on how many projects the schema enricher walks when building
/// the customfield union. Higher values trade richer cross-project
/// coverage for fatter `tools/list` payloads — 30 is empirically
/// enough to surface common agile fields on enterprise instances
/// without overflowing token budgets.
pub const MAX_ENRICHMENT_PROJECTS: usize = 30;

/// Strategy for choosing which Jira projects' metadata to fetch
/// when building the enricher cache. Different deployments need
/// different selection logic — a 5-project team wants `All`, a
/// 1000-project enterprise wants `RecentActivity` or
/// `Configured` so the enricher stays inside the
/// `MAX_ENRICHMENT_PROJECTS` budget.
#[derive(Debug, Clone)]
pub enum MetadataLoadStrategy {
    /// Explicit list of project keys (e.g. from app config).
    /// Fastest path: skips Jira-side filtering entirely.
    Configured(Vec<String>),
    /// Projects the authenticated user has interacted with most
    /// recently. Resolves through Jira's `/project/search?recent=N`
    /// (Cloud) or the `recent` flag on `/project` (Server/DC).
    /// Best default when the operator hasn't picked a list.
    MyProjects,
    /// Projects with issues updated in the last `days`. Uses a
    /// JQL search across `lastIssueUpdateTime`. Picks up
    /// stale-but-still-active projects that `MyProjects` may miss.
    RecentActivity { days: u32 },
    /// Every project the user can read. Errors out (with a
    /// suggestion to switch strategies) if total >
    /// [`MAX_ENRICHMENT_PROJECTS`] — loading 1000 projects'
    /// metadata is rarely what anyone wanted.
    All,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_metadata_load_strategy_variants_construct() {
        // Smoke test: every variant constructs with realistic args.
        // Concrete behaviour lives in `JiraClient::load_default_metadata`
        // strategy implementations (follow-up commits).
        let _configured = MetadataLoadStrategy::Configured(vec!["PROJ".into()]);
        let _my_projects = MetadataLoadStrategy::MyProjects;
        let _recent = MetadataLoadStrategy::RecentActivity { days: 90 };
        let _all = MetadataLoadStrategy::All;
    }

    fn sample_option_field() -> JiraCustomField {
        JiraCustomField {
            id: "customfield_10001".into(),
            name: "Sprint".into(),
            field_type: JiraFieldType::Option,
            required: false,
            options: vec![
                JiraFieldOption {
                    id: "1".into(),
                    name: "Sprint 1".into(),
                },
                JiraFieldOption {
                    id: "2".into(),
                    name: "Sprint 2".into(),
                },
            ],
        }
    }

    #[test]
    fn test_jira_option_transform() {
        let field = sample_option_field();
        assert_eq!(
            field.transform_value(&json!("Sprint 1")),
            json!({ "id": "1" })
        );
    }

    #[test]
    fn test_jira_option_case_insensitive() {
        let field = sample_option_field();
        assert_eq!(
            field.transform_value(&json!("sprint 2")),
            json!({ "id": "2" })
        );
    }

    #[test]
    fn test_jira_array_transform() {
        let field = JiraCustomField {
            id: "customfield_10002".into(),
            name: "Fix Versions".into(),
            field_type: JiraFieldType::Array,
            required: false,
            options: vec![
                JiraFieldOption {
                    id: "v1".into(),
                    name: "1.0".into(),
                },
                JiraFieldOption {
                    id: "v2".into(),
                    name: "2.0".into(),
                },
            ],
        };
        assert_eq!(
            field.transform_value(&json!(["1.0", "2.0"])),
            json!([{ "id": "v1" }, { "id": "v2" }])
        );
    }

    #[test]
    fn test_metadata_single_project() {
        let meta = JiraMetadata {
            flavor: JiraFlavor::Cloud,
            projects: [(
                "PROJ".into(),
                JiraProjectMetadata {
                    issue_types: vec![],
                    components: vec![],
                    priorities: vec![],
                    link_types: vec![],
                    custom_fields: vec![],
                },
            )]
            .into_iter()
            .collect(),
            structures: vec![],
        };
        assert!(meta.is_single_project());
    }

    #[test]
    fn test_metadata_all_issue_types_deduped() {
        let meta = JiraMetadata {
            flavor: JiraFlavor::Cloud,
            projects: [
                (
                    "PROJ".into(),
                    JiraProjectMetadata {
                        issue_types: vec![
                            JiraIssueType {
                                id: "1".into(),
                                name: "Task".into(),
                                subtask: false,
                            },
                            JiraIssueType {
                                id: "2".into(),
                                name: "Bug".into(),
                                subtask: false,
                            },
                            JiraIssueType {
                                id: "3".into(),
                                name: "Sub-task".into(),
                                subtask: true,
                            },
                        ],
                        components: vec![],
                        priorities: vec![],
                        link_types: vec![],
                        custom_fields: vec![],
                    },
                ),
                (
                    "INFRA".into(),
                    JiraProjectMetadata {
                        issue_types: vec![
                            JiraIssueType {
                                id: "1".into(),
                                name: "Task".into(),
                                subtask: false,
                            },
                            JiraIssueType {
                                id: "4".into(),
                                name: "Epic".into(),
                                subtask: false,
                            },
                        ],
                        components: vec![],
                        priorities: vec![],
                        link_types: vec![],
                        custom_fields: vec![],
                    },
                ),
            ]
            .into_iter()
            .collect(),
            structures: vec![],
        };
        let types = meta.all_issue_types();
        assert_eq!(types, vec!["Bug", "Epic", "Task"]); // sorted, deduped, no subtask
    }

    #[test]
    fn jira_metadata_deserialises_without_structures_field() {
        // Back-compat: pre-existing persisted metadata does not carry the
        // new `structures` field. `#[serde(default)]` must fill in an
        // empty vec so old payloads still round-trip cleanly.
        let raw = serde_json::json!({
            "flavor": "cloud",
            "projects": {}
        });
        let meta: JiraMetadata = serde_json::from_value(raw).unwrap();
        assert!(meta.structures.is_empty());
    }

    #[test]
    fn jira_metadata_roundtrips_structures_list() {
        let meta = JiraMetadata {
            flavor: JiraFlavor::Cloud,
            projects: Default::default(),
            structures: vec![
                JiraStructureRef {
                    id: 7,
                    name: "Q1 Planning".into(),
                    description: Some("Top-level roadmap".into()),
                },
                JiraStructureRef {
                    id: 42,
                    name: "Sprint Board".into(),
                    description: None,
                },
            ],
        };

        let json = serde_json::to_value(&meta).unwrap();
        // `description: None` is skipped on serialize for compactness.
        assert_eq!(json["structures"][1].get("description"), None);

        let restored: JiraMetadata = serde_json::from_value(json).unwrap();
        assert_eq!(restored.structures, meta.structures);
    }
}
