//! Tool enrichment traits and schema utilities.
//!
//! This module defines the `ToolEnricher` trait and `ToolSchema` struct
//! that enable dynamic modification of MCP tool schemas. Provider crates
//! implement `ToolEnricher` to adapt tool schemas to their capabilities.
//!
//! Three categories of enrichers use the same trait:
//! 1. **Provider enrichers** — adapt tools to provider capabilities
//! 2. **Pipeline enrichers** — add output control parameters
//! 3. **Custom enrichers** — third-party plugins

use serde_json::{json, Value};

/// Trait for plugins that dynamically modify tool schemas and transform arguments.
///
/// Enrichers are executed in registration order by the `Executor`.
pub trait ToolEnricher: Send + Sync {
    /// Which tools this enricher applies to.
    fn supported_tools(&self) -> &[&str];

    /// Modify the tool schema during `tools/list`.
    fn enrich_schema(&self, tool_name: &str, schema: &mut ToolSchema);

    /// Transform arguments before tool execution.
    fn transform_args(&self, tool_name: &str, args: &mut Value);
}

/// Mutable wrapper around a JSON Schema for a tool's input parameters.
///
/// Provides convenience methods for common enrichment operations.
/// Prefer enum strings over free-form strings to help LLMs.
#[derive(Debug, Clone)]
pub struct ToolSchema {
    pub properties: serde_json::Map<String, Value>,
    pub required: Vec<String>,
}

impl ToolSchema {
    /// Create from a JSON Schema value.
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

    /// Add a string parameter with enum values (preferred over free-form strings).
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
    }

    #[test]
    fn test_tool_schema_remove_params() {
        let mut schema = ToolSchema::from_json(&json!({
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
        let original = json!({
            "type": "object",
            "properties": { "title": { "type": "string" } },
            "required": ["title"],
        });
        let schema = ToolSchema::from_json(&original);
        let result = schema.to_json();
        assert_eq!(result["properties"]["title"]["type"], "string");
        assert_eq!(result["required"], json!(["title"]));
    }
}
