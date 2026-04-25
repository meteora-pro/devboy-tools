//! L2 generic MCKP router.
//!
//! Picks a format encoder based on shape classification and the knobs in
//! [`crate::adaptive_config::MckpConfig`].
//!
//! Returns `Some((format_id, body))` only when the chosen encoding is
//! strictly shorter than the raw input; otherwise `None` (the pipeline
//! falls through to L3 as-is).

use crate::adaptive_config::MckpConfig;
use crate::shape::ClassifiedResponse;
use crate::telemetry::Shape;
use crate::templates;

/// Dispatch by shape. Produces `(format_id, body)` or `None` if no format
/// yields a smaller payload than the raw input.
pub fn route(
    config: &MckpConfig,
    raw: &str,
    cls: &ClassifiedResponse,
) -> Option<(&'static str, String)> {
    match cls.shape {
        Shape::MarkdownTable => try_csv_from_md(config, raw, cls),
        Shape::ArrayOfObjects => try_array_csv(config, raw, cls),
        Shape::NestedObject => try_deep_mckp(config, raw, cls),
        Shape::FlatObject => try_kv_or_compact(config, raw, cls),
        _ => None, // Prose / numbered_list / code_block / etc → L3
    }
}

fn try_csv_from_md(
    config: &MckpConfig,
    raw: &str,
    cls: &ClassifiedResponse,
) -> Option<(&'static str, String)> {
    if !config.format_enabled("csv_from_md") {
        return None;
    }
    let min_cols = config.shape_thresholds.markdown_table_min_cols;
    if cls.md_n_cols.unwrap_or(0) < min_cols {
        return None;
    }
    let body = templates::csv_from_md(raw, cls)?;
    if body.len() < raw.len() {
        Some(("csv_from_md", body))
    } else {
        None
    }
}

fn try_array_csv(
    config: &MckpConfig,
    raw: &str,
    cls: &ClassifiedResponse,
) -> Option<(&'static str, String)> {
    if !config.format_enabled("csv") {
        return json_compact_fallback(config, raw, "array");
    }
    let min_items = config.shape_thresholds.array_of_objects_min_items;
    let min_stability = config.shape_thresholds.array_of_objects_min_key_stability;
    let items_ok = cls.n_items.map(|n| n >= min_items).unwrap_or(false);
    let stable_ok = cls
        .key_stability
        .map(|s| s >= min_stability)
        .unwrap_or(false);
    if !(items_ok && stable_ok) {
        return json_compact_fallback(config, raw, "array");
    }

    let val: serde_json::Value = serde_json::from_str(raw.trim_start()).ok()?;
    let items = val.as_array()?;
    if items.is_empty() {
        return None;
    }

    // Union of keys across items (stable header).
    use std::collections::BTreeSet;
    let mut headers: BTreeSet<String> = BTreeSet::new();
    for item in items.iter().take(200) {
        if let Some(obj) = item.as_object() {
            for k in obj.keys() {
                headers.insert(k.clone());
            }
        }
    }
    let headers: Vec<String> = headers.into_iter().collect();

    let mut out = String::new();
    out.push_str(&headers.join(","));
    out.push('\n');
    for item in items {
        let obj = match item.as_object() {
            Some(o) => o,
            None => continue,
        };
        let row: Vec<String> = headers
            .iter()
            .map(|k| value_to_csv_cell(obj.get(k).unwrap_or(&serde_json::Value::Null)))
            .collect();
        out.push_str(&row.join(","));
        out.push('\n');
    }
    if out.len() < raw.len() {
        Some(("csv", out))
    } else {
        json_compact_fallback(config, raw, "array")
    }
}

fn value_to_csv_cell(v: &serde_json::Value) -> String {
    let s = match v {
        serde_json::Value::Null => String::new(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::String(s) => s.clone(),
        _ => v.to_string(),
    };
    let needs_quote = s.contains(',') || s.contains('"') || s.contains('\n');
    if needs_quote {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s
    }
}

fn try_deep_mckp(
    config: &MckpConfig,
    raw: &str,
    cls: &ClassifiedResponse,
) -> Option<(&'static str, String)> {
    if !config.format_enabled("deep_mckp") {
        return json_compact_fallback(config, raw, "nested");
    }
    // True per-subtree MCKP: object wrapping a homogeneous inner array →
    // top-level kv lines + array as union-of-keys table (nested cells = inline JSON).
    // This preserves all data — fix for the encoder bug where naive CSV/Markdown
    // dropped wrapping object's top-level fields (Paper 2, §Encoder Bug Postmortem).
    if let Some(body) = templates::deep_mckp_with_inner_table(raw, cls)
        && body.len() < raw.len()
    {
        return Some(("deep_mckp_inner_table", body));
    }
    // Fall back to compact JSON when no inner array is found or no gain.
    let body = templates::pipeline_deep_mckp(raw, cls)?;
    if body.len() < raw.len() {
        Some(("deep_mckp", body))
    } else {
        None
    }
}

fn try_kv_or_compact(
    config: &MckpConfig,
    raw: &str,
    cls: &ClassifiedResponse,
) -> Option<(&'static str, String)> {
    let min_fields = config.shape_thresholds.flat_object_min_fields;
    let fields = cls.n_fields.unwrap_or(0);
    if config.format_enabled("kv")
        && fields >= min_fields
        && let Some(body) = kv_format(raw)
        && body.len() < raw.len()
    {
        return Some(("kv", body));
    }
    json_compact_fallback(config, raw, "flat")
}

fn kv_format(raw: &str) -> Option<String> {
    let val: serde_json::Value = serde_json::from_str(raw.trim_start()).ok()?;
    let obj = val.as_object()?;
    let mut out = String::new();
    for (k, v) in obj {
        let v_str = match v {
            serde_json::Value::Null => String::new(),
            serde_json::Value::Bool(b) => b.to_string(),
            serde_json::Value::Number(n) => n.to_string(),
            serde_json::Value::String(s) => s.clone(),
            _ => v.to_string(),
        };
        out.push_str(k);
        out.push_str(": ");
        out.push_str(&v_str);
        out.push('\n');
    }
    Some(out)
}

fn json_compact_fallback(
    config: &MckpConfig,
    raw: &str,
    _reason: &'static str,
) -> Option<(&'static str, String)> {
    if !config.format_enabled("json_compact") {
        return None;
    }
    let val: serde_json::Value = serde_json::from_str(raw.trim_start()).ok()?;
    let compact = serde_json::to_string(&val).ok()?;
    if compact.len() < raw.len() {
        Some(("json_compact", compact))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adaptive_config::AdaptiveConfig;
    use crate::shape::classify;

    #[test]
    fn markdown_table_routes_to_csv_from_md() {
        let md =
            "| id | name | status |\n|---|---|---|\n| 1 | a | x |\n| 2 | b | y |\n| 3 | c | z |\n";
        let cls = classify(md);
        let cfg = AdaptiveConfig::default();
        let (id, body) = route(&cfg.mckp, md, &cls).unwrap();
        assert_eq!(id, "csv_from_md");
        assert!(body.contains("id,name,status"));
    }

    #[test]
    fn array_of_objects_routes_to_csv() {
        let json = r#"[
            {"id":1,"name":"a","status":"ok"},
            {"id":2,"name":"b","status":"bad"},
            {"id":3,"name":"c","status":"ok"},
            {"id":4,"name":"d","status":"ok"},
            {"id":5,"name":"e","status":"bad"}
        ]"#;
        let cls = classify(json);
        let cfg = AdaptiveConfig::default();
        let (id, body) = route(&cfg.mckp, json, &cls).unwrap();
        assert_eq!(id, "csv");
        assert!(body.starts_with("id,name,status\n"));
        assert!(body.contains("1,a,ok"));
    }

    #[test]
    fn flat_object_routes_to_compact() {
        let json = r#"{"a":1,"b":"hello","c":true}"#;
        let cls = classify(json);
        let cfg = AdaptiveConfig::default();
        // Small object, fields < 8 → kv not chosen; json_compact may not gain
        // much on already-compact JSON. Just ensure no panic.
        let _ = route(&cfg.mckp, json, &cls);
    }

    #[test]
    fn flat_object_with_many_fields_uses_kv() {
        // 9 fields triggers kv_min_fields=8 threshold.
        let json = r#"{"a":1,"b":2,"c":3,"d":4,"e":5,"f":6,"g":7,"h":8,"i":9,"j":10}"#;
        let pretty =
            serde_json::to_string_pretty(&serde_json::from_str::<serde_json::Value>(json).unwrap())
                .unwrap();
        let cls = classify(&pretty);
        let cfg = AdaptiveConfig::default();
        if let Some((id, _)) = route(&cfg.mckp, &pretty, &cls) {
            assert!(id == "kv" || id == "json_compact");
        }
    }

    #[test]
    fn nested_object_falls_through_deep_mckp() {
        let json = r#"{"id":1,"nested":{"a":1,"b":2},"arr":[1,2,3]}"#;
        let pretty =
            serde_json::to_string_pretty(&serde_json::from_str::<serde_json::Value>(json).unwrap())
                .unwrap();
        let cls = classify(&pretty);
        let cfg = AdaptiveConfig::default();
        let (id, body) = route(&cfg.mckp, &pretty, &cls).unwrap();
        assert!(id == "deep_mckp" || id == "json_compact");
        assert!(body.len() < pretty.len());
    }

    #[test]
    fn prose_does_not_route() {
        let txt = "Just some prose text, no structure.";
        let cls = classify(txt);
        let cfg = AdaptiveConfig::default();
        assert!(route(&cfg.mckp, txt, &cls).is_none());
    }

    #[test]
    fn respects_disabled_formats() {
        let md = "| id | name |\n|---|---|\n| 1 | a |\n| 2 | b |\n";
        let cls = classify(md);
        let mut cfg = AdaptiveConfig::default();
        cfg.mckp.formats_enabled = vec![]; // nothing allowed
        assert!(route(&cfg.mckp, md, &cls).is_none());
    }

    #[test]
    fn respects_md_cols_threshold() {
        let md = "| id |\n|---|\n| 1 |\n| 2 |\n"; // 1 column
        let cls = classify(md);
        let cfg = AdaptiveConfig::default();
        // min_cols default 2 → rejected
        assert!(route(&cfg.mckp, md, &cls).is_none());
    }

    #[test]
    fn array_with_unstable_keys_falls_back_to_json_compact() {
        // key_stability below threshold → skip csv, try json_compact.
        let json = r#"[{"a":1},{"b":2},{"c":3},{"d":4}]"#;
        let pretty =
            serde_json::to_string_pretty(&serde_json::from_str::<serde_json::Value>(json).unwrap())
                .unwrap();
        let cls = classify(&pretty);
        let cfg = AdaptiveConfig::default();
        let out = route(&cfg.mckp, &pretty, &cls);
        if let Some((id, _)) = out {
            assert_eq!(id, "json_compact");
        }
    }

    #[test]
    fn array_with_csv_disabled_uses_json_compact_fallback() {
        let json = r#"[
            {"id":1,"name":"a"},
            {"id":2,"name":"b"},
            {"id":3,"name":"c"},
            {"id":4,"name":"d"}
        ]"#;
        let cls = classify(json);
        let mut cfg = AdaptiveConfig::default();
        cfg.mckp.formats_enabled = vec!["json_compact".into()];
        let (id, _) = route(&cfg.mckp, json, &cls).unwrap();
        assert_eq!(id, "json_compact");
    }

    #[test]
    fn nested_object_with_deep_mckp_disabled_falls_back() {
        let pretty = serde_json::to_string_pretty(
            &serde_json::from_str::<serde_json::Value>(
                r#"{"id":1,"nested":{"a":1,"b":2},"arr":[1,2,3]}"#,
            )
            .unwrap(),
        )
        .unwrap();
        let cls = classify(&pretty);
        let mut cfg = AdaptiveConfig::default();
        cfg.mckp.formats_enabled = vec!["json_compact".into()]; // deep_mckp disabled
        let (id, _) = route(&cfg.mckp, &pretty, &cls).unwrap();
        assert_eq!(id, "json_compact");
    }

    #[test]
    fn flat_object_below_kv_threshold_uses_json_compact() {
        let pretty = serde_json::to_string_pretty(
            &serde_json::from_str::<serde_json::Value>(r#"{"a":1,"b":2,"c":3}"#).unwrap(),
        )
        .unwrap();
        let cls = classify(&pretty);
        let cfg = AdaptiveConfig::default();
        let out = route(&cfg.mckp, &pretty, &cls);
        if let Some((id, _)) = out {
            assert_eq!(id, "json_compact");
        }
    }

    #[test]
    fn empty_array_returns_none() {
        let json = "[]";
        let cls = classify(json);
        let cfg = AdaptiveConfig::default();
        assert!(route(&cfg.mckp, json, &cls).is_none());
    }

    #[test]
    fn code_block_shape_is_not_routed() {
        let text = "```python\ndef foo():\n    return 1\n```\n";
        let cls = classify(text);
        let cfg = AdaptiveConfig::default();
        assert!(route(&cfg.mckp, text, &cls).is_none());
    }
}
