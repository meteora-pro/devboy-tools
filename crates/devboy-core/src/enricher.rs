//! Tool enrichment traits and schema utilities.
//!
//! This module defines the `ToolEnricher` trait and `ToolSchema` struct
//! that enable dynamic modification of MCP tool schemas. Provider crates
//! implement `ToolEnricher` to adapt tool schemas to their capabilities.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

use crate::tool_category::ToolCategory;
use crate::tool_value_model::ToolValueModel;

/// Trait for plugins that dynamically modify tool schemas and transform arguments.
///
/// Enrichers are executed in registration order by the `Executor`.
/// Each enricher declares which tool categories it supports — only tools
/// from those categories will be enriched and shown in `list_tools()`.
pub trait ToolEnricher: Send + Sync {
    /// Which tool categories this provider/enricher supports.
    /// Tools from other categories won't be shown when this enricher is active.
    fn supported_categories(&self) -> &[ToolCategory];

    /// Modify the tool schema during `tools/list`.
    fn enrich_schema(&self, tool_name: &str, schema: &mut ToolSchema);

    /// Transform arguments before tool execution.
    fn transform_args(&self, tool_name: &str, args: &mut Value);

    /// Optional: provider-shipped value model for `tool_name`. Returned
    /// models are merged into `AdaptiveConfig.tools` at startup so the
    /// Paper 3 enrichment planner can read them via
    /// `effective_tool_value_model`.
    ///
    /// Default impl returns `None` — built-in enrichers that do not
    /// participate in the planner can ignore the method entirely.
    fn value_model(&self, _tool_name: &str) -> Option<ToolValueModel> {
        None
    }

    /// Build the JSON arguments for a *speculatively pre-fetched*
    /// follow-up call.
    ///
    /// Given the tool that just produced `prev_result` (`prev_tool`),
    /// the follow-up tool's `FollowUpLink` (with `projection` /
    /// `projection_arg` set), the host asks the enricher: "what `args`
    /// should I pass to `<follow-up tool>`?"
    ///
    /// Returns:
    ///
    /// - `Some(json)` — emit one prefetch request per object in the
    ///   returned array (planner caps at `max_parallel_prefetches`).
    ///   Top-level shape is `[{ <args1> }, { <args2> }, …]`.
    /// - `None` (default) — provider has no opinion; the host falls
    ///   back to the generic projection in `link.projection_arg`.
    ///
    /// Built-in enrichers should override this for the high-volume
    /// follow-up chains identified in `paper3_corpus_findings.md`
    /// (Glob → Read, Grep → Read, WebSearch → WebFetch, …).
    fn project_args(
        &self,
        _prev_tool: &str,
        _prev_result: &Value,
        _link: &crate::tool_value_model::FollowUpLink,
    ) -> Option<Value> {
        None
    }

    /// Optional dynamic rate-limit host for `tool_name`, derived from
    /// runtime `args`. Provider returns the network host the call
    /// will hit (e.g. `Some("api.github.com")`) so the speculative
    /// dispatcher can cap concurrent in-flight prefetches per host.
    ///
    /// Default: `None` — host falls back to
    /// `ToolValueModel::rate_limit_host` (the static configuration
    /// value), and if that is also `None` the prefetch is uncapped.
    ///
    /// Override this for tools whose target host is per-call —
    /// `WebFetch` (host from `url` arg), `WebSearch` against multiple
    /// search engines, MCP wrappers around generic HTTP clients.
    fn rate_limit_host(&self, _tool_name: &str, _args: &Value) -> Option<String> {
        None
    }
}

/// JSON Schema property definition for a tool parameter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PropertySchema {
    /// JSON Schema type: "string", "number", "integer", "boolean", "array", "object".
    /// Empty when [`Self::any_of`] is set — JSON Schema treats `type`
    /// and `anyOf` as alternatives, and the serializer skips empty
    /// `type` on the wire so the rendered schema stays valid for
    /// LLM tool-call validators.
    #[serde(rename = "type", default, skip_serializing_if = "String::is_empty")]
    pub schema_type: String,

    /// Human-readable description of this parameter.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// Allowed values (enum constraint).
    #[serde(rename = "enum", skip_serializing_if = "Option::is_none")]
    pub enum_values: Option<Vec<String>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<Value>,

    /// Minimum value (for number/integer).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub minimum: Option<f64>,

    /// Maximum value (for number/integer).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub maximum: Option<f64>,

    /// Items schema (for array type).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub items: Option<Box<PropertySchema>>,

    /// Schema alternatives — used when a parameter accepts shapes
    /// that can't be unified under one `type` (e.g. a Jira
    /// customfield that's a select on Project A and free text on
    /// Project B). Mutually exclusive with `schema_type` per JSON
    /// Schema's `anyOf` semantics — when set, [`Self::schema_type`]
    /// is empty and the serializer skips it.
    #[serde(rename = "anyOf", default, skip_serializing_if = "Option::is_none")]
    pub any_of: Option<Vec<PropertySchema>>,

    /// Marker that this field was added/modified by an enricher.
    #[serde(rename = "x-enriched", skip_serializing_if = "Option::is_none")]
    pub enriched: Option<bool>,
}

impl PropertySchema {
    /// Create a string property.
    pub fn string(description: &str) -> Self {
        Self {
            schema_type: "string".into(),
            description: Some(description.into()),
            ..Default::default()
        }
    }

    /// Create a string property with enum values.
    pub fn string_enum(values: &[&str], description: &str) -> Self {
        Self {
            schema_type: "string".into(),
            description: Some(description.into()),
            enum_values: Some(values.iter().map(|s| s.to_string()).collect()),
            enriched: Some(true),
            ..Default::default()
        }
    }

    /// Create a number property.
    pub fn number(description: &str) -> Self {
        Self {
            schema_type: "number".into(),
            description: Some(description.into()),
            ..Default::default()
        }
    }

    /// Create an integer property with optional min/max.
    pub fn integer(description: &str, min: Option<f64>, max: Option<f64>) -> Self {
        Self {
            schema_type: "integer".into(),
            description: Some(description.into()),
            minimum: min,
            maximum: max,
            ..Default::default()
        }
    }

    /// Create a boolean property.
    pub fn boolean(description: &str) -> Self {
        Self {
            schema_type: "boolean".into(),
            description: Some(description.into()),
            ..Default::default()
        }
    }

    /// Create an array property with items schema.
    pub fn array(items: PropertySchema, description: &str) -> Self {
        Self {
            schema_type: "array".into(),
            description: Some(description.into()),
            items: Some(Box::new(items)),
            ..Default::default()
        }
    }

    /// Create a schema that accepts any of several alternatives —
    /// JSON Schema's `anyOf`. Used when a parameter can take
    /// shapes that don't fit under a single `type` (e.g. a custom
    /// field with different option lists across projects). The
    /// outer schema carries the description and `anyOf` array;
    /// `schema_type` is left empty so the wire format is a valid
    /// `anyOf`-only schema.
    pub fn any_of(description: &str, schemas: Vec<PropertySchema>) -> Self {
        Self {
            schema_type: String::new(),
            description: Some(description.into()),
            any_of: Some(schemas),
            enriched: Some(true),
            ..Default::default()
        }
    }
}

impl Default for PropertySchema {
    fn default() -> Self {
        Self {
            schema_type: "string".into(),
            description: None,
            enum_values: None,
            default: None,
            minimum: None,
            maximum: None,
            items: None,
            any_of: None,
            enriched: None,
        }
    }
}

/// Tool input schema with typed property definitions.
///
/// Represents a JSON Schema `{ type: "object", properties: {...}, required: [...] }`.
/// Uses `PropertySchema` for type-safe parameter definitions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSchema {
    /// Parameter definitions keyed by parameter name.
    pub properties: HashMap<String, PropertySchema>,
    /// List of required parameter names.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required: Vec<String>,
}

impl ToolSchema {
    /// Create an empty schema.
    pub fn new() -> Self {
        Self {
            properties: HashMap::new(),
            required: Vec::new(),
        }
    }

    /// Create from a JSON Schema value (for backward compatibility).
    pub fn from_json(schema: &Value) -> Self {
        serde_json::from_value::<ToolSchema>(schema.clone()).unwrap_or_else(|_| {
            // Fallback: manual parsing for non-standard JSON
            let properties = schema
                .get("properties")
                .and_then(|p| {
                    serde_json::from_value::<HashMap<String, PropertySchema>>(p.clone()).ok()
                })
                .unwrap_or_default();
            let required = schema
                .get("required")
                .and_then(|r| r.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            Self {
                properties,
                required,
            }
        })
    }

    /// Convert to a JSON Schema value.
    pub fn to_json(&self) -> Value {
        let mut schema = serde_json::json!({
            "type": "object",
            "properties": self.properties,
        });
        if !self.required.is_empty() {
            schema["required"] = serde_json::json!(self.required);
        }
        schema
    }

    /// Add a string parameter with enum values.
    pub fn add_enum_param(&mut self, name: &str, values: &[&str], description: &str) {
        self.properties.insert(
            name.into(),
            PropertySchema::string_enum(values, description),
        );
    }

    /// Set enum values on an existing parameter.
    pub fn set_enum(&mut self, param: &str, values: &[String]) {
        if let Some(prop) = self.properties.get_mut(param) {
            prop.enum_values = Some(values.to_vec());
            prop.enriched = Some(true);
        }
    }

    /// Add a typed property.
    pub fn add_property(&mut self, name: &str, prop: PropertySchema) {
        self.properties.insert(name.into(), prop);
    }

    /// Add a parameter with a raw JSON Schema value (backward compat).
    pub fn add_param(&mut self, name: &str, schema: Value) {
        if let Ok(prop) = serde_json::from_value::<PropertySchema>(schema) {
            self.properties.insert(name.into(), prop);
        }
    }

    /// Remove parameters not supported by the current provider.
    pub fn remove_params(&mut self, names: &[&str]) {
        for name in names {
            self.properties.remove(*name);
            self.required.retain(|r| r != *name);
        }
    }

    /// Set whether a parameter is required.
    pub fn set_required(&mut self, param: &str, required: bool) {
        if required {
            if !self.required.contains(&param.to_string()) {
                self.required.push(param.into());
            }
        } else {
            self.required.retain(|r| r != param);
        }
    }

    /// Update a parameter's description.
    pub fn set_description(&mut self, param: &str, desc: &str) {
        if let Some(prop) = self.properties.get_mut(param) {
            prop.description = Some(desc.into());
        }
    }

    /// Set a default value for a parameter.
    pub fn set_default(&mut self, param: &str, value: Value) {
        if let Some(prop) = self.properties.get_mut(param) {
            prop.default = Some(value);
        }
    }
}

impl Default for ToolSchema {
    fn default() -> Self {
        Self::new()
    }
}

/// Convert a human-readable field name to a safe `cf_` parameter name.
///
/// Examples:
/// - `"Story Points"` → `"cf_story_points"`
/// - `"Risk Level"` → `"cf_risk_level"`
pub fn sanitize_field_name(name: &str) -> String {
    let sanitized: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();
    let collapsed = sanitized
        .split('_')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("_");
    format!("cf_{collapsed}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sanitize_field_name() {
        assert_eq!(sanitize_field_name("Story Points"), "cf_story_points");
        assert_eq!(sanitize_field_name("Risk Level"), "cf_risk_level");
        assert_eq!(
            sanitize_field_name("My Custom Field!"),
            "cf_my_custom_field"
        );
        assert_eq!(sanitize_field_name("simple"), "cf_simple");
        // Non-ASCII becomes underscore
        assert_eq!(sanitize_field_name("Приоритет"), "cf_");
    }

    #[test]
    fn test_property_schema_constructors() {
        let s = PropertySchema::string("A description");
        assert_eq!(s.schema_type, "string");
        assert_eq!(s.description.as_deref(), Some("A description"));

        let e = PropertySchema::string_enum(&["a", "b"], "Pick one");
        assert_eq!(e.enum_values, Some(vec!["a".to_string(), "b".to_string()]));
        assert_eq!(e.enriched, Some(true));

        let n = PropertySchema::number("Count");
        assert_eq!(n.schema_type, "number");

        let i = PropertySchema::integer("Limit", Some(1.0), Some(100.0));
        assert_eq!(i.minimum, Some(1.0));
        assert_eq!(i.maximum, Some(100.0));

        let b = PropertySchema::boolean("Flag");
        assert_eq!(b.schema_type, "boolean");

        let a = PropertySchema::array(PropertySchema::string("item"), "List");
        assert_eq!(a.schema_type, "array");
        assert!(a.items.is_some());
    }

    /// `any_of` produces a JSON Schema with no top-level `type` —
    /// the wire shape is `{"description": ..., "anyOf": [...]}`,
    /// which is what JSON Schema validators expect for alternatives.
    #[test]
    fn test_property_schema_any_of_constructor() {
        let alt = PropertySchema::any_of(
            "Severity (varies per project)",
            vec![
                PropertySchema::string_enum(&["High", "Medium", "Low"], "Project A"),
                PropertySchema::string_enum(&["P1", "P2", "P3"], "Project B"),
            ],
        );
        assert_eq!(alt.schema_type, "");
        assert_eq!(
            alt.description.as_deref(),
            Some("Severity (varies per project)")
        );
        assert_eq!(alt.enriched, Some(true));
        let variants = alt.any_of.as_ref().expect("anyOf set");
        assert_eq!(variants.len(), 2);
        assert_eq!(variants[0].enum_values.as_ref().unwrap()[0], "High");
        assert_eq!(variants[1].enum_values.as_ref().unwrap()[0], "P1");
    }

    /// Empty `schema_type` is skipped during JSON serialisation so
    /// the rendered schema is valid `anyOf`-only — no stray
    /// `"type": ""` ending up on the wire. We check the parsed
    /// outer object specifically, since inner variants legitimately
    /// carry their own `type`.
    #[test]
    fn test_property_schema_any_of_serialization_omits_empty_type() {
        let alt = PropertySchema::any_of(
            "alt",
            vec![PropertySchema::string("a"), PropertySchema::number("b")],
        );
        let value = serde_json::to_value(&alt).unwrap();
        let obj = value.as_object().expect("object");
        assert!(
            !obj.contains_key("type"),
            "outer object must not have type: {value}"
        );
        assert!(obj.contains_key("anyOf"), "missing anyOf: {value}");
        // Inner variants keep their `type` — that's expected.
        let any_of = obj["anyOf"].as_array().unwrap();
        assert_eq!(any_of[0]["type"], "string");
        assert_eq!(any_of[1]["type"], "number");
    }

    #[test]
    fn test_tool_schema_add_enum_param() {
        let mut schema = ToolSchema::new();
        schema.add_enum_param("status", &["open", "closed"], "Issue status");
        let prop = schema.properties.get("status").unwrap();
        assert_eq!(prop.schema_type, "string");
        assert_eq!(
            prop.enum_values,
            Some(vec!["open".to_string(), "closed".to_string()])
        );
        assert_eq!(prop.enriched, Some(true));
    }

    #[test]
    fn test_tool_schema_remove_params() {
        let mut schema = ToolSchema::from_json(&serde_json::json!({
            "type": "object",
            "properties": {
                "title": { "type": "string" },
                "priority": { "type": "string" },
            },
            "required": ["title", "priority"],
        }));
        schema.remove_params(&["priority"]);
        assert!(!schema.properties.contains_key("priority"));
        assert_eq!(schema.required, vec!["title"]);
    }

    #[test]
    fn test_tool_schema_roundtrip() {
        let mut schema = ToolSchema::new();
        schema.add_property("title", PropertySchema::string("Title"));
        schema.set_required("title", true);

        let json = schema.to_json();
        assert_eq!(json["properties"]["title"]["type"], "string");
        assert_eq!(json["required"], serde_json::json!(["title"]));

        let restored = ToolSchema::from_json(&json);
        assert!(restored.properties.contains_key("title"));
        assert_eq!(restored.required, vec!["title"]);
    }

    #[test]
    fn test_tool_schema_set_enum() {
        let mut schema = ToolSchema::new();
        schema.add_property("state", PropertySchema::string("Filter by state"));
        schema.set_enum(
            "state",
            &["opened".into(), "closed".into(), "merged".into()],
        );
        let state = schema.properties.get("state").unwrap();
        assert_eq!(
            state.enum_values,
            Some(vec![
                "opened".to_string(),
                "closed".to_string(),
                "merged".to_string()
            ])
        );
        assert_eq!(state.enriched, Some(true));
        // Original description preserved
        assert_eq!(state.description.as_deref(), Some("Filter by state"));
    }

    #[test]
    fn test_tool_schema_set_required() {
        let mut schema = ToolSchema::new();
        schema.required = vec!["title".into()];

        schema.set_required("description", true);
        assert_eq!(schema.required, vec!["title", "description"]);

        schema.set_required("title", false);
        assert_eq!(schema.required, vec!["description"]);

        // Idempotent
        schema.set_required("description", true);
        assert_eq!(schema.required, vec!["description"]);
    }

    #[test]
    fn test_tool_schema_set_default() {
        let mut schema = ToolSchema::new();
        schema.add_property("limit", PropertySchema::integer("Max results", None, None));
        schema.set_default("limit", serde_json::json!(20));
        assert_eq!(
            schema.properties.get("limit").unwrap().default,
            Some(serde_json::json!(20))
        );
    }

    #[test]
    fn test_tool_schema_add_param_from_json() {
        let mut schema = ToolSchema::new();
        schema.add_param(
            "cf_risk",
            serde_json::json!({
                "type": "string",
                "enum": ["Low", "Medium", "High"],
                "description": "Risk level",
                "x-enriched": true,
            }),
        );
        let prop = schema.properties.get("cf_risk").unwrap();
        assert_eq!(prop.schema_type, "string");
        assert_eq!(
            prop.enum_values,
            Some(vec![
                "Low".to_string(),
                "Medium".to_string(),
                "High".to_string()
            ])
        );
    }

    #[test]
    fn test_from_json_backward_compat() {
        let json = serde_json::json!({
            "type": "object",
            "properties": {
                "state": {
                    "type": "string",
                    "enum": ["open", "closed"],
                    "description": "Issue state"
                },
                "limit": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 100
                }
            },
            "required": ["state"]
        });

        let schema = ToolSchema::from_json(&json);
        assert_eq!(schema.properties.len(), 2);
        assert_eq!(schema.required, vec!["state"]);

        let state = schema.properties.get("state").unwrap();
        assert_eq!(state.schema_type, "string");
        assert_eq!(
            state.enum_values,
            Some(vec!["open".to_string(), "closed".to_string()])
        );

        let limit = schema.properties.get("limit").unwrap();
        assert_eq!(limit.schema_type, "integer");
        assert_eq!(limit.minimum, Some(1.0));
        assert_eq!(limit.maximum, Some(100.0));
    }
}
