//! Tool signature matching — decides which upstream tools have local counterparts.
//!
//! The matcher runs at startup and on every upstream reconnect:
//!
//! 1. Collect the local tool catalogue (from `ToolHandler::available_tools()`).
//! 2. Collect the raw (unprefixed) tool catalogue from every connected upstream proxy.
//! 3. For every tool name present in both, check schema compatibility.
//!
//! # Cloud priority
//!
//! When a tool exists only upstream it stays remote — the local routing engine never
//! synthesizes a local handler. When it exists only locally it behaves like a regular
//! built-in tool (unchanged baseline behaviour). Only when both sides advertise the same
//! name is the routing engine allowed to consider dispatching locally.
//!
//! # Graceful degradation
//!
//! If schemas disagree in a way the matcher cannot reconcile, the routing engine is
//! expected to fall back to upstream for that tool (remote wins). The matcher flags the
//! mismatch with a human-readable reason so callers can surface it in logs or status
//! commands.

use std::collections::HashMap;

use serde_json::Value;

use crate::protocol::ToolDefinition;

/// Per-tool outcome of the matcher.
#[derive(Debug, Clone)]
pub struct ToolMatch {
    /// The unprefixed tool name (`get_issues`, not `cloud__get_issues`).
    pub tool_name: String,
    /// True if the tool is implemented by the local `ToolHandler`.
    pub local_present: bool,
    /// True if at least one upstream proxy advertises this tool.
    pub remote_present: bool,
    /// Only set when both sides are present. `Some(true)` means the local schema can
    /// satisfy every required argument the upstream schema declares.
    pub schema_compatible: Option<bool>,
    /// Prefix of the first upstream advertising this tool (used to build the fully
    /// qualified remote name `{prefix}__{tool_name}`).
    pub upstream_prefix: Option<String>,
    /// Human-readable mismatch description. Always `Some` when `schema_compatible ==
    /// Some(false)`; may also carry an advisory note when compatible but imperfect.
    pub schema_mismatch: Option<String>,
}

impl ToolMatch {
    /// Is there both a local and a remote implementation for this tool?
    pub fn is_matched(&self) -> bool {
        self.local_present && self.remote_present
    }

    /// Is the local executor a viable routing target for this tool?
    pub fn is_routable_local(&self) -> bool {
        self.is_matched() && self.schema_compatible.unwrap_or(false)
    }

    /// Fully qualified remote tool name (`{prefix}__{tool_name}`) if we know the prefix.
    pub fn prefixed_remote_name(&self) -> Option<String> {
        self.upstream_prefix
            .as_ref()
            .map(|p| format!("{}__{}", p, self.tool_name))
    }
}

/// Index of every tool name encountered across local and upstream catalogues.
#[derive(Debug, Clone, Default)]
pub struct MatchReport {
    pub matches: HashMap<String, ToolMatch>,
}

impl MatchReport {
    /// Tools that have a viable local counterpart (matched + compatible schema).
    pub fn routable_locally(&self) -> Vec<&ToolMatch> {
        self.matches.values().filter(|m| m.is_routable_local()).collect()
    }

    /// Tools that exist only remotely.
    pub fn remote_only(&self) -> Vec<&ToolMatch> {
        self.matches
            .values()
            .filter(|m| m.remote_present && !m.local_present)
            .collect()
    }

    /// Tools that exist only locally.
    pub fn local_only(&self) -> Vec<&ToolMatch> {
        self.matches
            .values()
            .filter(|m| m.local_present && !m.remote_present)
            .collect()
    }

    /// Matched pairs where schemas disagree (compatible=false).
    pub fn incompatible_pairs(&self) -> Vec<&ToolMatch> {
        self.matches
            .values()
            .filter(|m| m.is_matched() && m.schema_compatible == Some(false))
            .collect()
    }

    pub fn get(&self, tool_name: &str) -> Option<&ToolMatch> {
        self.matches.get(tool_name)
    }

    pub fn len(&self) -> usize {
        self.matches.len()
    }

    pub fn is_empty(&self) -> bool {
        self.matches.is_empty()
    }
}

/// Input for a matcher call — a local tool catalogue and one or more upstream catalogues.
pub struct ToolCatalogue<'a> {
    /// Local tool definitions with schemas (e.g., from `ToolHandler::available_tools()`).
    pub local: &'a [ToolDefinition],
    /// Upstream tool definitions per prefix. Each `(prefix, tools)` entry is one connected
    /// upstream proxy; the tool definitions must be raw (unprefixed).
    pub upstream: Vec<(String, &'a [ToolDefinition])>,
}

/// Build a match report by comparing the local and upstream tool catalogues.
pub fn build_report(catalogue: ToolCatalogue<'_>) -> MatchReport {
    let mut matches: HashMap<String, ToolMatch> = HashMap::new();

    for tool in catalogue.local {
        matches.insert(
            tool.name.clone(),
            ToolMatch {
                tool_name: tool.name.clone(),
                local_present: true,
                remote_present: false,
                schema_compatible: None,
                upstream_prefix: None,
                schema_mismatch: None,
            },
        );
    }

    for (prefix, upstream_tools) in &catalogue.upstream {
        for up_tool in *upstream_tools {
            let entry = matches
                .entry(up_tool.name.clone())
                .or_insert_with(|| ToolMatch {
                    tool_name: up_tool.name.clone(),
                    local_present: false,
                    remote_present: false,
                    schema_compatible: None,
                    upstream_prefix: None,
                    schema_mismatch: None,
                });

            entry.remote_present = true;
            if entry.upstream_prefix.is_none() {
                entry.upstream_prefix = Some(prefix.clone());
            }

            if entry.local_present
                && let Some(local_tool) =
                    catalogue.local.iter().find(|t| t.name == up_tool.name)
            {
                let check =
                    check_schema_compat(&local_tool.input_schema, &up_tool.input_schema);
                entry.schema_compatible = Some(check.is_compatible);
                entry.schema_mismatch = check.reason;
            }
        }
    }

    MatchReport { matches }
}

#[derive(Debug, Clone)]
struct SchemaCheck {
    is_compatible: bool,
    reason: Option<String>,
}

/// Conservative schema compatibility check.
///
/// Rules:
/// - Every field `upstream.required` declares must also exist in `local.properties`.
///   If missing, we mark incompatible (upstream arguments would be silently dropped).
/// - Extra fields in either direction are permitted.
/// - If the local schema declares a `required` field the upstream schema does not even
///   describe, we still mark compatible but surface an advisory note — the local
///   executor will enforce its own requirements at call time.
///
/// This intentionally ignores type checking for now. Structural type mismatches would
/// manifest as runtime errors from the local executor; we rely on `fallback_on_error`
/// to recover. A future revision can tighten this.
fn check_schema_compat(local: &Value, remote: &Value) -> SchemaCheck {
    let local_props = schema_properties(local);
    let local_required = schema_required(local);
    let remote_props = schema_properties(remote);
    let remote_required = schema_required(remote);

    for field in &remote_required {
        if !local_props.contains_key(field) {
            return SchemaCheck {
                is_compatible: false,
                reason: Some(format!(
                    "upstream requires `{}` which local schema does not declare",
                    field
                )),
            };
        }
    }

    for field in &local_required {
        if !remote_props.contains_key(field) && !remote_required.contains(field) {
            return SchemaCheck {
                is_compatible: true,
                reason: Some(format!(
                    "local requires `{}` which upstream schema does not describe; local enforcement still applies",
                    field
                )),
            };
        }
    }

    SchemaCheck {
        is_compatible: true,
        reason: None,
    }
}

fn schema_properties(schema: &Value) -> HashMap<String, &Value> {
    let Some(obj) = schema.as_object() else {
        return HashMap::new();
    };
    let Some(props) = obj.get("properties").and_then(|v| v.as_object()) else {
        return HashMap::new();
    };
    props.iter().map(|(k, v)| (k.clone(), v)).collect()
}

fn schema_required(schema: &Value) -> Vec<String> {
    schema
        .get("required")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tool(name: &str, schema: Value) -> ToolDefinition {
        ToolDefinition {
            name: name.to_string(),
            description: format!("tool {}", name),
            input_schema: schema,
            category: None,
        }
    }

    fn empty_schema() -> Value {
        json!({"type": "object", "properties": {}, "required": []})
    }

    #[test]
    fn test_build_report_all_matched_same_schema() {
        let local = vec![
            tool("get_issues", empty_schema()),
            tool("get_merge_requests", empty_schema()),
        ];
        let remote = vec![
            tool("get_issues", empty_schema()),
            tool("get_merge_requests", empty_schema()),
        ];

        let report = build_report(ToolCatalogue {
            local: &local,
            upstream: vec![("cloud".to_string(), &remote)],
        });

        assert_eq!(report.len(), 2);
        let m = report.get("get_issues").unwrap();
        assert!(m.is_matched());
        assert_eq!(m.schema_compatible, Some(true));
        assert_eq!(m.upstream_prefix.as_deref(), Some("cloud"));
        assert_eq!(m.prefixed_remote_name().as_deref(), Some("cloud__get_issues"));
    }

    #[test]
    fn test_build_report_local_only() {
        let local = vec![tool("list_contexts", empty_schema())];
        let report = build_report(ToolCatalogue {
            local: &local,
            upstream: vec![("cloud".to_string(), &[])],
        });

        let m = report.get("list_contexts").unwrap();
        assert!(m.local_present);
        assert!(!m.remote_present);
        assert!(!m.is_matched());
    }

    #[test]
    fn test_build_report_remote_only() {
        let remote = vec![tool("cloud_specific_tool", empty_schema())];
        let report = build_report(ToolCatalogue {
            local: &[],
            upstream: vec![("cloud".to_string(), &remote)],
        });

        let m = report.get("cloud_specific_tool").unwrap();
        assert!(m.remote_present);
        assert!(!m.local_present);
        assert_eq!(m.upstream_prefix.as_deref(), Some("cloud"));
    }

    #[test]
    fn test_schema_compat_missing_required_field_is_incompatible() {
        let local = vec![tool(
            "get_issue",
            json!({
                "type": "object",
                "properties": { "key": {"type": "string"} },
                "required": ["key"]
            }),
        )];
        let remote = vec![tool(
            "get_issue",
            json!({
                "type": "object",
                "properties": {
                    "key": {"type": "string"},
                    "workspace_id": {"type": "string"}
                },
                "required": ["key", "workspace_id"]
            }),
        )];

        let report = build_report(ToolCatalogue {
            local: &local,
            upstream: vec![("cloud".to_string(), &remote)],
        });

        let m = report.get("get_issue").unwrap();
        assert!(m.is_matched());
        assert_eq!(m.schema_compatible, Some(false));
        assert!(m.schema_mismatch.is_some());
        assert!(!m.is_routable_local());
    }

    #[test]
    fn test_schema_compat_extra_local_required_is_advisory_but_compatible() {
        let local = vec![tool(
            "get_issue",
            json!({
                "type": "object",
                "properties": {
                    "key": {"type": "string"},
                    "workspace_id": {"type": "string"}
                },
                "required": ["key", "workspace_id"]
            }),
        )];
        let remote = vec![tool(
            "get_issue",
            json!({
                "type": "object",
                "properties": { "key": {"type": "string"} },
                "required": ["key"]
            }),
        )];

        let report = build_report(ToolCatalogue {
            local: &local,
            upstream: vec![("cloud".to_string(), &remote)],
        });

        let m = report.get("get_issue").unwrap();
        assert_eq!(m.schema_compatible, Some(true));
        assert!(m.schema_mismatch.is_some());
        assert!(m.is_routable_local());
    }

    #[test]
    fn test_report_classification_helpers() {
        let local = vec![
            tool("local_only", empty_schema()),
            tool("both_matched", empty_schema()),
        ];
        let remote = vec![
            tool("remote_only", empty_schema()),
            tool("both_matched", empty_schema()),
        ];

        let report = build_report(ToolCatalogue {
            local: &local,
            upstream: vec![("up".to_string(), &remote)],
        });

        let local_only: Vec<&str> = report
            .local_only()
            .iter()
            .map(|m| m.tool_name.as_str())
            .collect();
        let remote_only: Vec<&str> = report
            .remote_only()
            .iter()
            .map(|m| m.tool_name.as_str())
            .collect();
        let routable: Vec<&str> = report
            .routable_locally()
            .iter()
            .map(|m| m.tool_name.as_str())
            .collect();

        assert_eq!(local_only, vec!["local_only"]);
        assert_eq!(remote_only, vec!["remote_only"]);
        assert_eq!(routable, vec!["both_matched"]);
    }

    #[test]
    fn test_first_upstream_wins_prefix_when_multiple_advertise_same_tool() {
        let a = vec![tool("shared", empty_schema())];
        let b = vec![tool("shared", empty_schema())];

        let report = build_report(ToolCatalogue {
            local: &[],
            upstream: vec![("cloudA".to_string(), &a), ("cloudB".to_string(), &b)],
        });

        assert_eq!(
            report.get("shared").unwrap().upstream_prefix.as_deref(),
            Some("cloudA")
        );
    }
}
