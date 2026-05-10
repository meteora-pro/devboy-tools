//! Jira schema enricher.
//!
//! Dynamic enricher supporting single-project and multi-project configurations.

use devboy_core::{
    CostModel, FollowUpLink, PropertySchema, SideEffectClass, ToolCategory, ToolEnricher,
    ToolSchema, ToolValueModel, ValueClass, sanitize_field_name,
};
use serde_json::{Value, json};

use crate::metadata::{JiraFieldType, JiraMetadata};

/// Dynamic schema enricher for Jira provider.
///
/// Single-project mode: removes `projectId`, adds enums, replaces customFields with cf_*.
/// Multi-project mode: keeps `projectId` as enum, shows known values in descriptions.
pub struct JiraSchemaEnricher {
    metadata: JiraMetadata,
    /// Precomputed category list: always advertises `IssueTracker`, and
    /// additionally advertises `JiraStructure` only when the metadata
    /// actually carries accessible structures. `Executor::list_tools` uses
    /// `supported_categories()` as a visibility filter, so returning
    /// `JiraStructure` unconditionally would surface the 9 Structure tools
    /// on Jira hosts without the plugin (pre-existing behaviour was to hide
    /// them because no enricher claimed the category).
    supported_categories: Vec<ToolCategory>,
}

impl JiraSchemaEnricher {
    /// Build an enricher from cached project metadata (custom fields,
    /// structures, statuses) used to refine MCP tool schemas at runtime.
    pub fn new(metadata: JiraMetadata) -> Self {
        let mut supported_categories = vec![ToolCategory::IssueTracker];
        if !metadata.structures.is_empty() {
            supported_categories.push(ToolCategory::JiraStructure);
        }
        Self {
            metadata,
            supported_categories,
        }
    }

    /// Enrich the `structureId` parameter on the 7 Structure tools that
    /// accept it. When the metadata carries a list of accessible structures
    /// we embed `id (name) — description` for each one in the parameter's
    /// description so an LLM sees the concrete IDs it can pick from.
    ///
    /// Note this is description-based enrichment, not a strict JSON Schema
    /// `enum`: `PropertySchema.enum_values` is `Option<Vec<String>>` today
    /// and widening that to support integer enums would be a cross-workspace
    /// breaking change. The description-based hint mirrors the existing
    /// priority-alias behaviour and is sufficient for LLM tool use; a
    /// strict enum can follow when `PropertySchema` grows a typed enum
    /// value type.
    ///
    /// When `metadata.structures` is empty the category is not advertised
    /// in [`Self::supported_categories`] so this method will not be
    /// invoked — but we still short-circuit defensively.
    fn enrich_structure_id(&self, schema: &mut ToolSchema) {
        if self.metadata.structures.is_empty() {
            return;
        }

        let mut entries: Vec<&crate::metadata::JiraStructureRef> =
            self.metadata.structures.iter().collect();
        entries.sort_by_key(|s| s.id);

        let list = entries
            .iter()
            .map(|s| match s.description.as_deref() {
                Some(desc) if !desc.is_empty() => format!("{} ({}) — {}", s.id, s.name, desc),
                _ => format!("{} ({})", s.id, s.name),
            })
            .collect::<Vec<_>>()
            .join(", ");

        let desc = format!(
            "Structure ID. Must be one of the accessible structures: {list}. Pick the numeric ID (the part before parentheses).",
        );
        schema.set_description("structureId", &desc);
    }
}

const REMOVE_PARAMS: &[&str] = &["points"];
const GET_ISSUES_REMOVE_PARAMS: &[&str] = &["stateCategory"];

/// Every tool in `ToolCategory::JiraStructure`. Structure-category schemas
/// have a different parameter surface from IssueTracker tools (no
/// `projectId`, no custom fields, etc), so the enricher routes them to a
/// dedicated branch and skips the IssueTracker enrichment path entirely —
/// even for `get_structures` and `create_structure`, which have no
/// `structureId` to enrich but also nothing for the IssueTracker branch to
/// say about them.
const STRUCTURE_TOOLS: &[&str] = &[
    "get_structures",
    "get_structure_forest",
    "add_structure_rows",
    "move_structure_rows",
    "remove_structure_row",
    "get_structure_values",
    "get_structure_views",
    "save_structure_view",
    "create_structure",
];

/// Subset of [`STRUCTURE_TOOLS`] that carry a `structureId` parameter —
/// the only ones that receive description enrichment today.
const STRUCTURE_ID_TOOLS: &[&str] = &[
    "get_structure_forest",
    "add_structure_rows",
    "move_structure_rows",
    "remove_structure_row",
    "get_structure_values",
    "get_structure_views",
    "save_structure_view",
];

impl ToolEnricher for JiraSchemaEnricher {
    fn supported_categories(&self) -> &[ToolCategory] {
        &self.supported_categories
    }

    fn enrich_schema(&self, tool_name: &str, schema: &mut ToolSchema) {
        // Structure-category tools early-return regardless of whether they
        // take `structureId` — the IssueTracker branch below would try to
        // `remove_params(REMOVE_PARAMS)` / apply `priority`/`issueType` enums
        // on schemas that do not own those parameters. Only the
        // `structureId`-taking subset receives description enrichment today;
        // `get_structures` and `create_structure` simply pass through.
        if STRUCTURE_TOOLS.contains(&tool_name) {
            if STRUCTURE_ID_TOOLS.contains(&tool_name) {
                self.enrich_structure_id(schema);
            }
            return;
        }

        schema.remove_params(REMOVE_PARAMS);

        if tool_name == "get_issues" {
            schema.remove_params(GET_ISSUES_REMOVE_PARAMS);
        }

        let is_single = self.metadata.is_single_project();

        // Project key handling
        if is_single {
            schema.remove_params(&["projectId"]);
        } else {
            let keys: Vec<String> = self
                .metadata
                .project_keys()
                .iter()
                .map(|k| k.to_string())
                .collect();
            if !keys.is_empty() {
                schema.set_enum("projectId", &keys);
                let desc = format!("REQUIRED. Jira project key. Available: {}", keys.join(", "));
                schema.set_description("projectId", &desc);
                schema.set_required("projectId", true);
            }
        }

        // Issue types enum (non-subtask)
        let issue_types = self.metadata.all_issue_types();
        if !issue_types.is_empty() {
            schema.set_enum("issueType", &issue_types);
            let desc = format!("Issue type. Available: {}", issue_types.join(", "));
            schema.set_description("issueType", &desc);
        }

        // Priorities enum with alias hints
        let priorities = self.metadata.all_priorities();
        if !priorities.is_empty() {
            schema.set_enum("priority", &priorities);
            let desc = format!(
                "Priority. Available: {}. Aliases: urgent\u{2192}Highest, high\u{2192}High, normal\u{2192}Medium, low\u{2192}Low",
                priorities.join(", ")
            );
            schema.set_description("priority", &desc);
        }

        // Components enum
        let components = self.metadata.all_components();
        if !components.is_empty() {
            schema.set_enum("components", &components);
            let desc = format!("Components. Available: {}", components.join(", "));
            schema.set_description("components", &desc);
        }

        // Link types
        if tool_name == "link_issues" {
            let link_types = self.metadata.all_link_types();
            if !link_types.is_empty() {
                let values: Vec<&str> = link_types.iter().map(|s| s.as_str()).collect();
                schema.add_enum_param("link_type", &values, "Issue link type");
            }
        }

        // Customfields: union across projects, capped by
        // `MAX_ENRICHMENT_PROJECTS`. Single-project and multi-
        // project modes share one path — the difference is
        // `custom_field_groups()` returns one entry per name in
        // single-project mode, multiple entries (one per project
        // carrying that name) in multi-project mode.
        if tool_name == "create_issue" || tool_name == "update_issue" {
            let groups = self.metadata.custom_field_groups();
            if !groups.is_empty() {
                schema.remove_params(&["customFields"]);

                for (name, variants) in &groups {
                    let representative = &variants[0];
                    let conflict = variants
                        .iter()
                        .skip(1)
                        .any(|v| v.field_type != representative.field_type);

                    match well_known_alias(name) {
                        Some(alias) => {
                            // Replace the raw `cf_*` slot with a
                            // canonical typed parameter — agents work
                            // with `epicKey` / `sprintId` /
                            // `epicName` instead of the instance-
                            // specific `customfield_NNNNN` id.
                            let alias_schema = well_known_alias_schema(alias, representative);
                            schema.add_param(alias, alias_schema);
                        }
                        None if conflict => {
                            // Cross-project shape conflict — emit a
                            // JSON Schema `anyOf` listing each
                            // project's variant so an LLM-side
                            // validator accepts whichever shape
                            // matches the target project.
                            let param_name = sanitize_field_name(name);
                            let sub_schemas: Vec<PropertySchema> = variants
                                .iter()
                                .map(|cf| {
                                    let raw = jira_custom_field_to_schema(cf);
                                    serde_json::from_value(raw).unwrap_or_default()
                                })
                                .collect();
                            let desc = format!(
                                "Custom field: {} (varies per project — {} shapes detected).",
                                name,
                                sub_schemas.len()
                            );
                            schema.add_param(
                                &param_name,
                                serde_json::to_value(PropertySchema::any_of(&desc, sub_schemas))
                                    .unwrap_or_default(),
                            );
                        }
                        None => {
                            let param_name = sanitize_field_name(name);
                            let field_schema = jira_custom_field_to_schema(representative);
                            schema.add_param(&param_name, field_schema);
                        }
                    }
                }
            }
        }
    }

    fn transform_args(&self, tool_name: &str, args: &mut Value) {
        if tool_name != "create_issue" && tool_name != "update_issue" {
            return;
        }

        // Transform priority aliases
        if let Some(obj) = args.as_object_mut()
            && let Some(priority) = obj.get("priority").and_then(|v| v.as_str())
        {
            let mapped = match priority {
                "urgent" => "Highest",
                "high" => "High",
                "normal" => "Medium",
                "low" => "Low",
                other => other,
            };
            obj.insert("priority".into(), json!(mapped));
        }

        // Transform aliases / cf_* params back to instance-specific
        // `customField` ids. In multi-project mode we resolve via the
        // project named in `args.projectId` — same display name can
        // map to different `customfield_*` ids across projects, so we
        // can't pick the first project blindly.
        let Some(obj) = args.as_object_mut() else {
            return;
        };

        let project_key = obj
            .get("projectId")
            .and_then(|v| v.as_str())
            .map(str::to_string);

        // Fields list to scan for: prefer the named project's
        // metadata; fall back to first project (covers single-project
        // and the `projectId`-omitted edge case).
        let project_fields: Vec<crate::metadata::JiraCustomField> = match project_key.as_deref() {
            Some(key) => self
                .metadata
                .projects
                .get(key)
                .map(|p| p.custom_fields.clone())
                .or_else(|| {
                    self.metadata
                        .projects
                        .values()
                        .next()
                        .map(|p| p.custom_fields.clone())
                })
                .unwrap_or_default(),
            None => self
                .metadata
                .projects
                .values()
                .next()
                .map(|p| p.custom_fields.clone())
                .unwrap_or_default(),
        };
        if project_fields.is_empty() {
            return;
        }

        let mut custom_fields = serde_json::Map::new();
        let mut cf_keys_to_remove: Vec<String> = Vec::new();

        for field in &project_fields {
            let param_name: String = match well_known_alias(&field.name) {
                Some(alias) => alias.to_string(),
                None => sanitize_field_name(&field.name),
            };
            if let Some(value) = obj.get(&param_name) {
                let transformed = field.transform_value(value);
                custom_fields.insert(field.id.clone(), transformed);
                cf_keys_to_remove.push(param_name);
            }
        }

        for key in cf_keys_to_remove {
            obj.remove(&key);
        }
        if !custom_fields.is_empty() {
            obj.insert("customFields".into(), Value::Object(custom_fields));
        }
    }

    /// Paper 3 — Jira read-only chains (issues / comments). Issue
    /// detail and comments are the highest-value follow-ups; create /
    /// update / transition are mutating and never speculatable.
    fn value_model(&self, tool_name: &str) -> Option<ToolValueModel> {
        let model = match tool_name {
            "get_issues" => ToolValueModel {
                value_class: ValueClass::Supporting,
                cost_model: CostModel {
                    typical_kb: 4.0,
                    max_kb: Some(40.0),
                    latency_ms_p50: Some(450),
                    freshness_ttl_s: Some(60),
                    ..CostModel::default()
                },
                follow_up: vec![
                    FollowUpLink {
                        tool: "get_issue".into(),
                        probability: 0.55,
                        projection: Some("key".into()),
                        projection_arg: Some("key".into()),
                    },
                    FollowUpLink {
                        tool: "get_issue_comments".into(),
                        probability: 0.45,
                        projection: Some("key".into()),
                        projection_arg: Some("key".into()),
                    },
                ],
                side_effect_class: SideEffectClass::ReadOnly,
                ..ToolValueModel::default()
            },
            "get_issue" => ToolValueModel {
                value_class: ValueClass::Critical,
                cost_model: CostModel {
                    typical_kb: 1.5,
                    latency_ms_p50: Some(220),
                    freshness_ttl_s: Some(60),
                    ..CostModel::default()
                },
                follow_up: vec![FollowUpLink {
                    tool: "get_issue_comments".into(),
                    probability: 0.50,
                    projection: Some("key".into()),
                    projection_arg: Some("key".into()),
                }],
                side_effect_class: SideEffectClass::ReadOnly,
                ..ToolValueModel::default()
            },
            "get_issue_comments" => ToolValueModel {
                value_class: ValueClass::Critical,
                cost_model: CostModel {
                    typical_kb: 2.5,
                    latency_ms_p50: Some(280),
                    freshness_ttl_s: Some(60),
                    ..CostModel::default()
                },
                side_effect_class: SideEffectClass::ReadOnly,
                ..ToolValueModel::default()
            },
            "create_issue" | "update_issue" | "add_issue_comment" | "link_issues"
            | "transition_issue" => ToolValueModel {
                value_class: ValueClass::Supporting,
                cost_model: CostModel {
                    typical_kb: 0.6,
                    latency_ms_p50: Some(380),
                    ..CostModel::default()
                },
                side_effect_class: SideEffectClass::MutatesExternal,
                ..ToolValueModel::default()
            },
            _ => return None,
        };
        Some(model)
    }

    /// Jira hosts vary per deployment (Cloud vs Server vs Data
    /// Center). Operators set `rate_limit_host` per-tool in TOML for
    /// shared rate-limit grouping; we don't statically assume Cloud.
    fn rate_limit_host(&self, _tool_name: &str, _args: &Value) -> Option<String> {
        None
    }
}

/// Map a customfield display name to the canonical alias the
/// enricher exposes in the schema. Keeps agents off the
/// instance-specific `customfield_NNNNN` ids for the few well-known
/// agile fields that have a stable name. Returns `None` for every
/// other customfield — those fall back to `cf_<sanitized_name>`.
fn well_known_alias(field_name: &str) -> Option<&'static str> {
    match field_name {
        "Epic Link" => Some("epicKey"),
        "Sprint" => Some("sprintId"),
        "Epic Name" => Some("epicName"),
        _ => None,
    }
}

/// Build the JSON Schema fragment for a well-known alias. Keeps the
/// shape opinionated (`epicKey` is always a string, `sprintId` always
/// a number) so the agent doesn't have to inspect the raw Jira
/// field type.
fn well_known_alias_schema(alias: &str, field: &crate::metadata::JiraCustomField) -> Value {
    match alias {
        "epicKey" => json!({
            "type": "string",
            "description": format!(
                "Parent epic issue key (e.g. \"PROJ-12\"). Maps to the Jira `Epic Link` customfield ({}) on this instance.",
                field.id
            ),
            "x-enriched": true,
        }),
        "sprintId" => json!({
            "type": "integer",
            "description": format!(
                "Numeric sprint id. Use `get_board_sprints` to discover available ids. Maps to the Jira `Sprint` customfield ({}) on this instance.",
                field.id
            ),
            "x-enriched": true,
        }),
        "epicName" => json!({
            "type": "string",
            "description": format!(
                "Epic Name — required when creating an Epic on Server/DC and Cloud company-managed projects. Maps to the Jira `Epic Name` customfield ({}) on this instance.",
                field.id
            ),
            "x-enriched": true,
        }),
        _ => jira_custom_field_to_schema(field),
    }
}

/// Convert a Jira custom field definition to a JSON Schema property.
fn jira_custom_field_to_schema(field: &crate::metadata::JiraCustomField) -> Value {
    match field.field_type {
        JiraFieldType::Option => {
            let options: Vec<&str> = field.options.iter().map(|o| o.name.as_str()).collect();
            json!({
                "type": "string",
                "enum": options,
                "description": format!("Custom field: {} (select). Choose one option.", field.name),
                "x-enriched": true,
            })
        }
        JiraFieldType::Array => {
            let options: Vec<&str> = field.options.iter().map(|o| o.name.as_str()).collect();
            json!({
                "type": "array",
                "items": { "type": "string", "enum": options },
                "description": format!("Custom field: {} (multi-select). Choose one or more.", field.name),
                "x-enriched": true,
            })
        }
        JiraFieldType::Number => json!({
            "type": "number",
            "description": format!("Custom field: {} (number).", field.name),
            "x-enriched": true,
        }),
        JiraFieldType::Date => json!({
            "type": "string",
            "description": format!("Custom field: {} (date, YYYY-MM-DD).", field.name),
            "x-enriched": true,
        }),
        JiraFieldType::DateTime => json!({
            "type": "string",
            "description": format!("Custom field: {} (datetime, ISO 8601).", field.name),
            "x-enriched": true,
        }),
        JiraFieldType::String | JiraFieldType::Any => json!({
            "type": "string",
            "description": format!("Custom field: {} (text).", field.name),
            "x-enriched": true,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::*;
    use std::collections::HashMap;

    fn single_project_metadata() -> JiraMetadata {
        let mut projects = HashMap::new();
        projects.insert(
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
                priorities: vec![
                    JiraPriority {
                        id: "1".into(),
                        name: "Highest".into(),
                    },
                    JiraPriority {
                        id: "2".into(),
                        name: "High".into(),
                    },
                    JiraPriority {
                        id: "3".into(),
                        name: "Medium".into(),
                    },
                    JiraPriority {
                        id: "4".into(),
                        name: "Low".into(),
                    },
                ],
                components: vec![
                    JiraComponent {
                        id: "10".into(),
                        name: "API".into(),
                    },
                    JiraComponent {
                        id: "11".into(),
                        name: "Frontend".into(),
                    },
                ],
                link_types: vec![JiraLinkType {
                    id: "1".into(),
                    name: "Blocks".into(),
                    outward: Some("blocks".into()),
                    inward: Some("is blocked by".into()),
                }],
                custom_fields: vec![JiraCustomField {
                    id: "customfield_10001".into(),
                    name: "Story Points".into(),
                    field_type: JiraFieldType::Number,
                    required: false,
                    options: vec![],
                }],
            },
        );
        JiraMetadata {
            flavor: JiraFlavor::Cloud,
            projects,
            structures: vec![],
        }
    }

    #[test]
    fn test_jira_enricher_single_project_removes_project_id() {
        let enricher = JiraSchemaEnricher::new(single_project_metadata());
        let mut schema = ToolSchema::from_json(&json!({
            "type": "object",
            "properties": {
                "projectId": { "type": "string" },
                "issueType": { "type": "string" },
                "priority": { "type": "string" },
            },
        }));

        enricher.enrich_schema("create_issue", &mut schema);

        assert!(!schema.properties.contains_key("projectId"));
        assert_eq!(
            schema.properties["issueType"].enum_values,
            Some(vec!["Bug".into(), "Task".into()]) // sorted, no Sub-task
        );
        assert_eq!(
            schema.properties["priority"].enum_values,
            Some(vec![
                "High".into(),
                "Highest".into(),
                "Low".into(),
                "Medium".into()
            ]) // sorted
        );
    }

    #[test]
    fn test_jira_enricher_adds_custom_fields() {
        let enricher = JiraSchemaEnricher::new(single_project_metadata());
        let mut schema = ToolSchema::from_json(&json!({
            "type": "object",
            "properties": {
                "customFields": { "type": "object" },
            },
        }));

        enricher.enrich_schema("create_issue", &mut schema);

        assert!(!schema.properties.contains_key("customFields"));
        assert!(schema.properties.contains_key("cf_story_points"));
        assert_eq!(schema.properties["cf_story_points"].schema_type, "number");
    }

    #[test]
    fn test_jira_enricher_transform_priority_alias() {
        let enricher = JiraSchemaEnricher::new(single_project_metadata());
        let mut args = json!({ "title": "Test", "priority": "urgent" });

        enricher.transform_args("create_issue", &mut args);

        assert_eq!(args["priority"], "Highest");
    }

    fn agile_metadata() -> JiraMetadata {
        // Project metadata that mimics a Jira-Software-enabled
        // instance: the three well-known agile customfields are
        // present, alongside an unrelated number field that should
        // keep falling back to `cf_*`.
        let mut projects = HashMap::new();
        projects.insert(
            "PROJ".into(),
            JiraProjectMetadata {
                issue_types: vec![],
                priorities: vec![],
                components: vec![],
                link_types: vec![],
                custom_fields: vec![
                    JiraCustomField {
                        id: "customfield_10014".into(),
                        name: "Epic Link".into(),
                        field_type: JiraFieldType::Any,
                        required: false,
                        options: vec![],
                    },
                    JiraCustomField {
                        id: "customfield_10020".into(),
                        name: "Sprint".into(),
                        field_type: JiraFieldType::Any,
                        required: false,
                        options: vec![],
                    },
                    JiraCustomField {
                        id: "customfield_10011".into(),
                        name: "Epic Name".into(),
                        field_type: JiraFieldType::String,
                        required: false,
                        options: vec![],
                    },
                    JiraCustomField {
                        id: "customfield_10001".into(),
                        name: "Story Points".into(),
                        field_type: JiraFieldType::Number,
                        required: false,
                        options: vec![],
                    },
                ],
            },
        );
        JiraMetadata {
            flavor: JiraFlavor::Cloud,
            projects,
            structures: vec![],
        }
    }

    /// When the project metadata carries Epic Link / Sprint / Epic
    /// Name customfields, the enricher exposes them under canonical
    /// aliases (`epicKey` / `sprintId` / `epicName`) instead of the
    /// raw `cf_epic_link` / `cf_sprint` / `cf_epic_name` slots.
    /// Other customfields stay on `cf_*`.
    #[test]
    fn test_jira_enricher_promotes_well_known_customfields_to_canonical_aliases() {
        let enricher = JiraSchemaEnricher::new(agile_metadata());
        let mut schema = ToolSchema::from_json(&json!({
            "type": "object",
            "properties": { "customFields": { "type": "object" } },
        }));

        enricher.enrich_schema("create_issue", &mut schema);

        assert!(schema.properties.contains_key("epicKey"));
        assert_eq!(schema.properties["epicKey"].schema_type, "string");
        assert!(schema.properties.contains_key("sprintId"));
        assert_eq!(schema.properties["sprintId"].schema_type, "integer");
        assert!(schema.properties.contains_key("epicName"));
        assert_eq!(schema.properties["epicName"].schema_type, "string");
        // Unrelated customfields still surface as `cf_*`.
        assert!(schema.properties.contains_key("cf_story_points"));
        // Raw aliases are NOT exposed (avoiding duplication).
        assert!(!schema.properties.contains_key("cf_epic_link"));
        assert!(!schema.properties.contains_key("cf_sprint"));
        assert!(!schema.properties.contains_key("cf_epic_name"));
    }

    /// `transform_args` translates canonical aliases back to the
    /// instance-specific customfield ids the Jira REST API expects.
    #[test]
    fn test_jira_enricher_transforms_canonical_aliases_to_customfield_ids() {
        let enricher = JiraSchemaEnricher::new(agile_metadata());
        let mut args = json!({
            "title": "Story under epic",
            "epicKey": "PROJ-1",
            "sprintId": 42,
            "epicName": "Q4 platform",
        });

        enricher.transform_args("create_issue", &mut args);

        assert!(args.get("epicKey").is_none());
        assert!(args.get("sprintId").is_none());
        assert!(args.get("epicName").is_none());
        let cf = args.get("customFields").expect("customFields object");
        assert_eq!(cf["customfield_10014"], "PROJ-1");
        assert_eq!(cf["customfield_10020"], 42);
        assert_eq!(cf["customfield_10011"], "Q4 platform");
    }

    /// Without the corresponding customfield in metadata, the
    /// enricher does NOT add `epicKey` / `sprintId` / `epicName` to
    /// the schema — agents see exactly what the instance supports.
    #[test]
    fn test_jira_enricher_omits_alias_when_customfield_absent() {
        // `single_project_metadata()` only has the Story Points
        // customfield, no Epic Link / Sprint / Epic Name.
        let enricher = JiraSchemaEnricher::new(single_project_metadata());
        let mut schema = ToolSchema::from_json(&json!({
            "type": "object",
            "properties": { "customFields": { "type": "object" } },
        }));

        enricher.enrich_schema("create_issue", &mut schema);

        assert!(!schema.properties.contains_key("epicKey"));
        assert!(!schema.properties.contains_key("sprintId"));
        assert!(!schema.properties.contains_key("epicName"));
    }

    #[test]
    fn test_jira_enricher_transform_custom_fields() {
        let enricher = JiraSchemaEnricher::new(single_project_metadata());
        let mut args = json!({
            "title": "Test",
            "cf_story_points": 8,
        });

        enricher.transform_args("create_issue", &mut args);

        assert!(args.get("cf_story_points").is_none());
        assert_eq!(args["customFields"]["customfield_10001"], 8);
    }

    #[test]
    fn test_jira_enricher_multi_project_keeps_project_id() {
        let mut meta = single_project_metadata();
        meta.projects.insert(
            "INFRA".into(),
            JiraProjectMetadata {
                issue_types: vec![],
                priorities: vec![],
                components: vec![],
                link_types: vec![],
                custom_fields: vec![],
            },
        );
        let enricher = JiraSchemaEnricher::new(meta);
        let mut schema = ToolSchema::from_json(&json!({
            "type": "object",
            "properties": {
                "projectId": { "type": "string" },
                "customFields": { "type": "object" },
            },
        }));

        enricher.enrich_schema("create_issue", &mut schema);

        // projectId kept as enum
        assert!(schema.properties.contains_key("projectId"));
        let project_enum = schema.properties["projectId"].enum_values.as_ref().unwrap();
        assert!(project_enum.contains(&"PROJ".to_string()));
        assert!(project_enum.contains(&"INFRA".to_string()));

        // customFields IS replaced in multi-project — the union of
        // customfields across projects gets expanded to typed
        // params. `customFields` raw escape-hatch goes away when
        // any project has customfields.
        assert!(!schema.properties.contains_key("customFields"));
        assert!(schema.properties.contains_key("cf_story_points"));
    }

    #[test]
    fn test_jira_enricher_transform_args_skips_non_create() {
        let enricher = JiraSchemaEnricher::new(single_project_metadata());
        let mut args = json!({"priority": "urgent"});
        enricher.transform_args("get_issues", &mut args);
        // Should not transform priority for get_issues
        assert_eq!(args["priority"], "urgent");
    }

    #[test]
    fn test_jira_enricher_transform_args_normal_priority() {
        let enricher = JiraSchemaEnricher::new(single_project_metadata());
        let mut args = json!({"title": "T", "priority": "normal"});
        enricher.transform_args("create_issue", &mut args);
        assert_eq!(args["priority"], "Medium");
    }

    #[test]
    fn test_jira_enricher_transform_args_non_alias_priority() {
        let enricher = JiraSchemaEnricher::new(single_project_metadata());
        let mut args = json!({"title": "T", "priority": "Highest"});
        enricher.transform_args("create_issue", &mut args);
        assert_eq!(args["priority"], "Highest"); // pass-through
    }

    /// When the same customfield name has different shapes across
    /// projects (e.g. `Severity` is a dropdown in PROJ but free
    /// text in INFRA), the enricher emits a JSON Schema `anyOf`
    /// listing each variant instead of silently picking one.
    #[test]
    fn test_jira_enricher_multi_project_conflict_emits_any_of() {
        let mut meta = single_project_metadata();
        // PROJ has Severity as Option (dropdown).
        meta.projects
            .get_mut("PROJ")
            .unwrap()
            .custom_fields
            .push(JiraCustomField {
                id: "customfield_50001".into(),
                name: "Severity".into(),
                field_type: JiraFieldType::Option,
                required: false,
                options: vec![
                    crate::metadata::JiraFieldOption {
                        id: "1".into(),
                        name: "High".into(),
                    },
                    crate::metadata::JiraFieldOption {
                        id: "2".into(),
                        name: "Low".into(),
                    },
                ],
            });
        // INFRA has Severity as plain String.
        meta.projects.insert(
            "INFRA".into(),
            JiraProjectMetadata {
                issue_types: vec![],
                priorities: vec![],
                components: vec![],
                link_types: vec![],
                custom_fields: vec![JiraCustomField {
                    id: "customfield_60001".into(),
                    name: "Severity".into(),
                    field_type: JiraFieldType::String,
                    required: false,
                    options: vec![],
                }],
            },
        );

        let enricher = JiraSchemaEnricher::new(meta);
        let mut schema = ToolSchema::from_json(&json!({
            "type": "object",
            "properties": { "customFields": { "type": "object" } },
        }));

        enricher.enrich_schema("create_issue", &mut schema);

        let severity = schema
            .properties
            .get("cf_severity")
            .expect("cf_severity present");
        assert_eq!(severity.schema_type, "");
        let variants = severity.any_of.as_ref().expect("anyOf set");
        assert_eq!(variants.len(), 2);
        // One variant must be the dropdown (with options), the
        // other a plain string. Order isn't guaranteed.
        let has_dropdown = variants.iter().any(|v| v.enum_values.is_some());
        let has_plain_string = variants
            .iter()
            .any(|v| v.schema_type == "string" && v.enum_values.is_none());
        assert!(has_dropdown, "missing dropdown variant: {variants:?}");
        assert!(
            has_plain_string,
            "missing plain-string variant: {variants:?}"
        );
    }

    /// Multi-project mode: `transform_args` resolves customfield ids
    /// against the project named in `args.projectId` — the same
    /// display name maps to different `customfield_*` ids across
    /// projects on real instances.
    #[test]
    fn test_jira_enricher_multi_project_resolves_per_project_id() {
        let mut meta = single_project_metadata();
        // PROJ has Story Points = customfield_10001 (from
        // single_project_metadata). INFRA gets the same display
        // name with a different id — exactly the case where
        // first-project resolution would target the wrong
        // customfield.
        meta.projects.insert(
            "INFRA".into(),
            JiraProjectMetadata {
                issue_types: vec![],
                priorities: vec![],
                components: vec![],
                link_types: vec![],
                custom_fields: vec![JiraCustomField {
                    id: "customfield_20001".into(),
                    name: "Story Points".into(),
                    field_type: JiraFieldType::Number,
                    required: false,
                    options: vec![],
                }],
            },
        );
        let enricher = JiraSchemaEnricher::new(meta);

        // projectId=INFRA → INFRA's customfield_20001
        let mut args = json!({"title": "T", "projectId": "INFRA", "cf_story_points": 5});
        enricher.transform_args("create_issue", &mut args);
        assert!(args.get("cf_story_points").is_none());
        assert_eq!(args["customFields"]["customfield_20001"], 5);
        assert!(args["customFields"].get("customfield_10001").is_none());

        // projectId=PROJ → PROJ's customfield_10001
        let mut args = json!({"title": "T", "projectId": "PROJ", "cf_story_points": 9});
        enricher.transform_args("create_issue", &mut args);
        assert!(args.get("cf_story_points").is_none());
        assert_eq!(args["customFields"]["customfield_10001"], 9);
        assert!(args["customFields"].get("customfield_20001").is_none());
    }

    /// `all_custom_fields` truncates above
    /// `MAX_ENRICHMENT_PROJECTS` so a 1000-project instance
    /// doesn't blow up the schema. Caller is expected to feed the
    /// most relevant subset.
    #[test]
    fn test_jira_metadata_caps_custom_fields_at_max_projects() {
        use crate::metadata::MAX_ENRICHMENT_PROJECTS;
        let mut meta = single_project_metadata();
        // Inflate to one over the cap with each project carrying a
        // unique customfield. Only the first MAX_ENRICHMENT_PROJECTS
        // (including the original PROJ) should make it into the
        // union.
        let extra = MAX_ENRICHMENT_PROJECTS;
        for i in 0..extra {
            meta.projects.insert(
                format!("EXTRA_{i}"),
                JiraProjectMetadata {
                    issue_types: vec![],
                    priorities: vec![],
                    components: vec![],
                    link_types: vec![],
                    custom_fields: vec![JiraCustomField {
                        id: format!("customfield_30{i:03}"),
                        name: format!("ExtraField{i}"),
                        field_type: JiraFieldType::String,
                        required: false,
                        options: vec![],
                    }],
                },
            );
        }
        // metadata now has 1 + MAX_ENRICHMENT_PROJECTS projects.
        let union = meta.all_custom_fields();
        // At most MAX_ENRICHMENT_PROJECTS projects' customfields
        // surface — exact set is HashMap-iteration-order-dependent
        // but the count is bounded.
        assert!(
            union.len() <= MAX_ENRICHMENT_PROJECTS,
            "union size {} exceeded cap {}",
            union.len(),
            MAX_ENRICHMENT_PROJECTS
        );
    }

    #[test]
    fn test_jira_enricher_components_enum() {
        let enricher = JiraSchemaEnricher::new(single_project_metadata());
        let mut schema = ToolSchema::from_json(&json!({
            "type": "object",
            "properties": {
                "components": { "type": "array" }
            }
        }));
        enricher.enrich_schema("create_issue", &mut schema);
        let comp = schema.properties.get("components").unwrap();
        assert_eq!(
            comp.enum_values,
            Some(vec!["API".into(), "Frontend".into()])
        );
    }

    #[test]
    fn test_jira_enricher_link_types() {
        let enricher = JiraSchemaEnricher::new(single_project_metadata());
        let mut schema = ToolSchema::new();
        enricher.enrich_schema("link_issues", &mut schema);
        let lt = schema.properties.get("link_type").unwrap();
        assert_eq!(lt.enum_values, Some(vec!["Blocks".into()]));
    }

    #[test]
    fn test_jira_enricher_get_issues_removes_state_category() {
        let enricher = JiraSchemaEnricher::new(single_project_metadata());
        let mut schema = ToolSchema::from_json(&json!({
            "type": "object",
            "properties": {
                "state": { "type": "string" },
                "stateCategory": { "type": "string" }
            }
        }));
        enricher.enrich_schema("get_issues", &mut schema);
        assert!(!schema.properties.contains_key("stateCategory"));
        assert!(schema.properties.contains_key("state"));
    }

    // -------------------------------------------------------------------------
    // Structure tool enrichment (depends on JiraMetadata.structures)
    // -------------------------------------------------------------------------

    fn metadata_with_structures(refs: Vec<crate::metadata::JiraStructureRef>) -> JiraMetadata {
        let mut meta = single_project_metadata();
        meta.structures = refs;
        meta
    }

    fn structureid_schema() -> ToolSchema {
        ToolSchema::from_json(&json!({
            "type": "object",
            "properties": {
                "structureId": {
                    "type": "integer",
                    "description": "Structure ID. Use get_structures to find it."
                }
            },
            "required": ["structureId"],
        }))
    }

    #[test]
    fn jira_enricher_does_not_advertise_jira_structure_when_no_structures() {
        // `Executor::list_tools` uses `supported_categories()` as a visibility
        // filter. Advertising `JiraStructure` unconditionally would surface
        // the 9 Structure tools on Jira hosts without the plugin — a
        // regression relative to pre-patch behaviour, where no enricher
        // claimed the category and the tools were hidden.
        let enricher = JiraSchemaEnricher::new(single_project_metadata());
        let categories = enricher.supported_categories();
        assert!(categories.contains(&ToolCategory::IssueTracker));
        assert!(!categories.contains(&ToolCategory::JiraStructure));
    }

    #[test]
    fn jira_enricher_advertises_jira_structure_when_metadata_has_structures() {
        let enricher = JiraSchemaEnricher::new(metadata_with_structures(vec![
            crate::metadata::JiraStructureRef {
                id: 1,
                name: "Only One".into(),
                description: None,
            },
        ]));
        let categories = enricher.supported_categories();
        assert!(categories.contains(&ToolCategory::IssueTracker));
        assert!(categories.contains(&ToolCategory::JiraStructure));
    }

    #[test]
    fn jira_enricher_populates_structureid_description_for_all_seven_tools() {
        let enricher = JiraSchemaEnricher::new(metadata_with_structures(vec![
            crate::metadata::JiraStructureRef {
                id: 1,
                name: "Q1 Planning".into(),
                description: Some("Quarter 1 plan".into()),
            },
            crate::metadata::JiraStructureRef {
                id: 7,
                name: "Sprint Board".into(),
                description: None,
            },
        ]));

        for tool in [
            "get_structure_forest",
            "add_structure_rows",
            "move_structure_rows",
            "remove_structure_row",
            "get_structure_values",
            "get_structure_views",
            "save_structure_view",
        ] {
            let mut schema = structureid_schema();
            enricher.enrich_schema(tool, &mut schema);

            let prop = schema.properties.get("structureId").unwrap();
            let desc = prop.description.as_deref().unwrap_or("");
            assert!(
                desc.contains("Must be one of the accessible structures"),
                "tool={tool} desc={desc}",
            );
            assert!(
                desc.contains("1 (Q1 Planning) — Quarter 1 plan"),
                "tool={tool}"
            );
            assert!(desc.contains("7 (Sprint Board)"), "tool={tool}");
        }
    }

    #[test]
    fn jira_enricher_sorts_structures_by_id_in_description() {
        let enricher = JiraSchemaEnricher::new(metadata_with_structures(vec![
            crate::metadata::JiraStructureRef {
                id: 42,
                name: "Roadmap".into(),
                description: None,
            },
            crate::metadata::JiraStructureRef {
                id: 1,
                name: "Q1".into(),
                description: None,
            },
            crate::metadata::JiraStructureRef {
                id: 7,
                name: "Sprint".into(),
                description: None,
            },
        ]));

        let mut schema = structureid_schema();
        enricher.enrich_schema("get_structure_forest", &mut schema);

        let desc = schema.properties["structureId"]
            .description
            .clone()
            .unwrap();
        let idx_1 = desc.find("1 (Q1)").expect("id 1 missing");
        let idx_7 = desc.find("7 (Sprint)").expect("id 7 missing");
        let idx_42 = desc.find("42 (Roadmap)").expect("id 42 missing");
        assert!(
            idx_1 < idx_7 && idx_7 < idx_42,
            "structures not sorted: {desc}"
        );
    }

    #[test]
    fn jira_enricher_leaves_schema_untouched_when_no_structures() {
        let enricher = JiraSchemaEnricher::new(metadata_with_structures(vec![]));

        let mut schema = structureid_schema();
        let original_desc = schema.properties["structureId"]
            .description
            .clone()
            .unwrap();

        enricher.enrich_schema("get_structure_forest", &mut schema);

        let desc_after = schema.properties["structureId"]
            .description
            .clone()
            .unwrap();
        assert_eq!(desc_after, original_desc);
    }

    #[test]
    fn jira_enricher_does_not_touch_get_structures_or_create_structure() {
        // These two tools carry the `JiraStructure` category but do not take
        // `structureId`. Previously the IssueTracker branch still ran on
        // them (because the early-return only covered the 7 id-taking
        // tools), which risked mutating unrelated params. After the fix
        // both should pass through unchanged: the enricher must not invent
        // `structureId` AND must not mutate parameters the Structure tools
        // actually own (e.g. `name` on `create_structure`).
        let enricher = JiraSchemaEnricher::new(metadata_with_structures(vec![
            crate::metadata::JiraStructureRef {
                id: 1,
                name: "One".into(),
                description: None,
            },
        ]));

        for tool in ["get_structures", "create_structure"] {
            let mut schema = ToolSchema::from_json(&json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "original" },
                    "points": { "type": "integer", "description": "must survive" }
                }
            }));
            enricher.enrich_schema(tool, &mut schema);
            assert!(
                !schema.properties.contains_key("structureId"),
                "enricher inserted structureId on {tool}",
            );
            // `points` is in REMOVE_PARAMS for the IssueTracker branch —
            // must stay intact here because we early-return for
            // Structure-category tools.
            assert!(
                schema.properties.contains_key("points"),
                "enricher dropped `points` on {tool} — IssueTracker branch leaked into Structure handling",
            );
            assert_eq!(
                schema.properties["name"].description.as_deref(),
                Some("original"),
                "enricher mutated `name` description on {tool}",
            );
        }
    }

    #[test]
    fn jira_enricher_skips_issuetracker_branch_for_structure_tools() {
        // Structure schemas have different parameters from IssueTracker tools
        // (e.g. no `points`). The Structure branch must early-return so the
        // IssueTracker `remove_params`/enum steps do not accidentally mutate
        // unrelated parameters.
        let enricher = JiraSchemaEnricher::new(metadata_with_structures(vec![
            crate::metadata::JiraStructureRef {
                id: 1,
                name: "One".into(),
                description: None,
            },
        ]));

        let mut schema = ToolSchema::from_json(&json!({
            "type": "object",
            "properties": {
                "structureId": { "type": "integer", "description": "old" },
                "points": { "type": "integer", "description": "must survive" }
            }
        }));
        enricher.enrich_schema("get_structure_forest", &mut schema);

        assert!(schema.properties.contains_key("points"));
    }
}
