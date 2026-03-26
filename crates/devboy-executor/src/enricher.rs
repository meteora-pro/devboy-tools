use serde_json::{json, Value};

use crate::output::ToolOutput;

/// Trait for plugins that dynamically modify tool schemas and transform arguments.
///
/// Three categories of enrichers use this same trait:
///
/// 1. **Provider enrichers** — adapt tools to provider capabilities
///    (e.g., ClickUp adds custom field params, GitHub removes unsupported params)
///
/// 2. **Pipeline enrichers** — add output control parameters
///    (e.g., `format` enum with markdown/compact/json)
///
/// 3. **Custom enrichers** — third-party plugins can implement this trait
///
/// Enrichers are executed in registration order.
pub trait ToolEnricher: Send + Sync {
    /// Which tools this enricher applies to.
    fn supported_tools(&self) -> &[&str];

    /// Modify the tool schema during `tools/list`.
    /// Called for each tool in `supported_tools()`.
    fn enrich_schema(&self, tool_name: &str, schema: &mut ToolSchema);

    /// Transform arguments before tool execution.
    /// E.g., convert `cf_story_points: 5` to `customFields: { "customfield_10001": 5 }`.
    fn transform_args(&self, tool_name: &str, args: &mut Value);

    /// Transform output after tool execution (optional, default is no-op).
    fn transform_output(&self, _tool_name: &str, _output: &mut ToolOutput) {}
}

/// Mutable wrapper around a JSON Schema for a tool's input parameters.
///
/// Provides convenience methods for common enrichment operations.
/// All methods prefer enum strings over free-form strings to help LLMs
/// understand available options.
#[derive(Debug, Clone)]
pub struct ToolSchema {
    pub properties: serde_json::Map<String, Value>,
    pub required: Vec<String>,
}

impl ToolSchema {
    /// Create from a JSON Schema `inputSchema` value.
    pub fn from_json(schema: &Value) -> Self {
        let properties = schema
            .get("properties")
            .and_then(|p| p.as_object())
            .cloned()
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
    }

    /// Convert back to a JSON Schema value.
    pub fn to_json(&self) -> Value {
        let mut schema = json!({
            "type": "object",
            "properties": Value::Object(self.properties.clone()),
        });
        if !self.required.is_empty() {
            schema["required"] = json!(self.required);
        }
        schema
    }

    /// Add a string parameter with enum values.
    /// Preferred over free-form strings — helps LLMs pick valid options.
    pub fn add_enum_param(&mut self, name: &str, values: &[&str], description: &str) {
        self.properties.insert(
            name.to_string(),
            json!({
                "type": "string",
                "enum": values,
                "description": description,
                "x-enriched": true,
            }),
        );
    }

    /// Set enum values on an existing parameter.
    pub fn set_enum(&mut self, param: &str, values: &[String]) {
        if let Some(prop) = self.properties.get_mut(param) {
            if let Some(obj) = prop.as_object_mut() {
                obj.insert("enum".into(), json!(values));
                obj.insert("x-enriched".into(), json!(true));
            }
        }
    }

    /// Add a parameter with a full JSON Schema definition.
    pub fn add_param(&mut self, name: &str, schema: Value) {
        self.properties.insert(name.to_string(), schema);
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
                self.required.push(param.to_string());
            }
        } else {
            self.required.retain(|r| r != param);
        }
    }

    /// Update a parameter's description.
    pub fn set_description(&mut self, param: &str, desc: &str) {
        if let Some(prop) = self.properties.get_mut(param) {
            if let Some(obj) = prop.as_object_mut() {
                obj.insert("description".into(), json!(desc));
            }
        }
    }

    /// Set a default value for a parameter.
    pub fn set_default(&mut self, param: &str, value: Value) {
        if let Some(prop) = self.properties.get_mut(param) {
            if let Some(obj) = prop.as_object_mut() {
                obj.insert("default".into(), value);
            }
        }
    }
}

/// Convert a human-readable field name to a safe `cf_` parameter name.
///
/// Examples:
/// - `"Story Points"` → `"cf_story_points"`
/// - `"Risk Level"` → `"cf_risk_level"`
/// - `"My Custom Field!"` → `"cf_my_custom_field"`
pub fn sanitize_field_name(name: &str) -> String {
    let sanitized: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();
    // Collapse multiple underscores and trim
    let collapsed = sanitized
        .split('_')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("_");
    format!("cf_{collapsed}")
}

// --- Built-in enrichers ---

/// Pipeline format enricher — adds `format` enum parameter to list tools.
pub struct PipelineFormatEnricher;

const LIST_TOOLS: &[&str] = &[
    "get_issues",
    "get_issue",
    "get_issue_comments",
    "get_merge_requests",
    "get_merge_request",
    "get_merge_request_discussions",
    "get_merge_request_diffs",
];

impl ToolEnricher for PipelineFormatEnricher {
    fn supported_tools(&self) -> &[&str] {
        LIST_TOOLS
    }

    fn enrich_schema(&self, _tool_name: &str, schema: &mut ToolSchema) {
        schema.add_enum_param(
            "format",
            &["markdown", "compact", "json"],
            "Output format. Default: markdown",
        );
    }

    fn transform_args(&self, _tool_name: &str, _args: &mut Value) {
        // format is consumed by the pipeline layer, not the provider
    }
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
        assert_eq!(
            sanitize_field_name("  multiple   spaces  "),
            "cf_multiple_spaces"
        );
    }

    #[test]
    fn test_tool_schema_add_enum_param() {
        let mut schema = ToolSchema {
            properties: serde_json::Map::new(),
            required: vec![],
        };
        schema.add_enum_param("status", &["open", "closed"], "Issue status");

        let prop = schema.properties.get("status").unwrap();
        assert_eq!(prop["type"], "string");
        assert_eq!(prop["enum"], json!(["open", "closed"]));
        assert_eq!(prop["x-enriched"], true);
    }

    #[test]
    fn test_tool_schema_remove_params() {
        let mut schema = ToolSchema::from_json(&json!({
            "type": "object",
            "properties": {
                "title": { "type": "string" },
                "priority": { "type": "string" },
                "components": { "type": "array" },
            },
            "required": ["title", "priority"],
        }));

        schema.remove_params(&["priority", "components"]);

        assert!(schema.properties.contains_key("title"));
        assert!(!schema.properties.contains_key("priority"));
        assert!(!schema.properties.contains_key("components"));
        assert_eq!(schema.required, vec!["title"]);
    }

    #[test]
    fn test_tool_schema_set_required() {
        let mut schema = ToolSchema {
            properties: serde_json::Map::new(),
            required: vec!["title".into()],
        };

        schema.set_required("description", true);
        assert_eq!(schema.required, vec!["title", "description"]);

        schema.set_required("title", false);
        assert_eq!(schema.required, vec!["description"]);

        // Idempotent
        schema.set_required("description", true);
        assert_eq!(schema.required, vec!["description"]);
    }

    #[test]
    fn test_tool_schema_set_enum() {
        let mut schema = ToolSchema::from_json(&json!({
            "type": "object",
            "properties": {
                "state": { "type": "string", "description": "Filter by state" },
            },
        }));

        schema.set_enum(
            "state",
            &["opened".into(), "closed".into(), "merged".into()],
        );

        let state = schema.properties.get("state").unwrap();
        assert_eq!(state["enum"], json!(["opened", "closed", "merged"]));
        assert_eq!(state["x-enriched"], true);
        // Original description preserved
        assert_eq!(state["description"], "Filter by state");
    }

    #[test]
    fn test_tool_schema_roundtrip() {
        let original = json!({
            "type": "object",
            "properties": {
                "title": { "type": "string" },
            },
            "required": ["title"],
        });

        let schema = ToolSchema::from_json(&original);
        let result = schema.to_json();

        assert_eq!(result["properties"]["title"]["type"], "string");
        assert_eq!(result["required"], json!(["title"]));
    }

    #[test]
    fn test_pipeline_format_enricher() {
        let enricher = PipelineFormatEnricher;

        assert!(enricher.supported_tools().contains(&"get_issues"));
        assert!(enricher.supported_tools().contains(&"get_merge_requests"));

        let mut schema = ToolSchema {
            properties: serde_json::Map::new(),
            required: vec![],
        };
        enricher.enrich_schema("get_issues", &mut schema);

        let format = schema.properties.get("format").unwrap();
        assert_eq!(format["enum"], json!(["markdown", "compact", "json"]));
    }

    #[test]
    fn test_tool_schema_set_default() {
        let mut schema = ToolSchema::from_json(&json!({
            "type": "object",
            "properties": {
                "limit": { "type": "number", "description": "Max results" },
            },
        }));

        schema.set_default("limit", json!(20));

        let limit = schema.properties.get("limit").unwrap();
        assert_eq!(limit["default"], 20);
    }
}
