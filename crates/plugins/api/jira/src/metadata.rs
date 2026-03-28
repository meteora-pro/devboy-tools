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
    /// Available components.
    #[serde(default)]
    pub components: Vec<JiraComponent>,
    /// Available priorities.
    #[serde(default)]
    pub priorities: Vec<JiraPriority>,
    /// Available issue link types.
    #[serde(default)]
    pub link_types: Vec<JiraLinkType>,
    /// Custom fields for this project.
    #[serde(default)]
    pub custom_fields: Vec<JiraCustomField>,
}

/// Jira issue type.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JiraIssueType {
    pub id: String,
    pub name: String,
    /// Whether this is a subtask type (exclude from create_issue enum).
    #[serde(default)]
    pub subtask: bool,
}

/// Jira component.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JiraComponent {
    pub id: String,
    pub name: String,
}

/// Jira priority.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JiraPriority {
    pub id: String,
    pub name: String,
}

/// Jira issue link type.
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
    /// Field type.
    pub field_type: JiraFieldType,
    /// Whether this field is required.
    #[serde(default)]
    pub required: bool,
    /// Options for option/array fields.
    #[serde(default)]
    pub options: Vec<JiraFieldOption>,
}

/// Jira custom field types.
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
        };
        let types = meta.all_issue_types();
        assert_eq!(types, vec!["Bug", "Epic", "Task"]); // sorted, deduped, no subtask
    }
}
