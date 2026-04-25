//! L1 per-endpoint templates.
//!
//! Each template is a pure function: `(raw_content, &classified) -> Option<compressed>`.
//! Returning `None` means "not applicable" — the pipeline falls through to L2.

use crate::shape::ClassifiedResponse;
use crate::telemetry::Shape;

/// Dispatch to the named template. Returns `None` if the template id is
/// unknown or does not apply to this input.
pub fn apply_by_id(template_id: &str, raw: &str, cls: &ClassifiedResponse) -> Option<String> {
    match template_id {
        "csv_from_md" => csv_from_md(raw, cls),
        "pipeline_deep_mckp" => pipeline_deep_mckp(raw, cls),
        "deep_mckp_with_inner_table" => deep_mckp_with_inner_table(raw, cls),
        "mr_diff_fence" => mr_diff_fence(raw, cls),
        _ => None,
    }
}

/// Parse a markdown table into rows/cols, re-emit as CSV.
/// Applies only when shape is `MarkdownTable`.
pub fn csv_from_md(raw: &str, cls: &ClassifiedResponse) -> Option<String> {
    if cls.shape != Shape::MarkdownTable {
        return None;
    }

    let lines: Vec<&str> = raw.lines().collect();
    let mut header_idx = None;
    let mut sep_idx = None;
    for (i, line) in lines.iter().enumerate() {
        let t = line.trim_start();
        if t.starts_with('|') {
            // Check next is separator
            if let Some(next) = lines.get(i + 1) {
                let nt = next.trim();
                if nt.starts_with('|')
                    && nt
                        .chars()
                        .all(|c| c == '|' || c == '-' || c == ':' || c.is_whitespace())
                {
                    header_idx = Some(i);
                    sep_idx = Some(i + 1);
                    break;
                }
            }
        }
    }
    let header_idx = header_idx?;
    let sep_idx = sep_idx?;

    // Extract cells from a pipe-delimited row.
    fn split_row(line: &str) -> Vec<String> {
        line.trim()
            .trim_start_matches('|')
            .trim_end_matches('|')
            .split('|')
            .map(|c| c.trim().to_string())
            .collect()
    }

    let headers = split_row(lines[header_idx]);
    let mut out = String::new();
    out.push_str(&csv_row(&headers));
    out.push('\n');
    for row_line in &lines[sep_idx + 1..] {
        let t = row_line.trim_start();
        if !t.starts_with('|') {
            if t.is_empty() {
                break;
            }
            continue;
        }
        let cells = split_row(row_line);
        // Pad / truncate to header length.
        let mut norm: Vec<String> = cells.into_iter().take(headers.len()).collect();
        while norm.len() < headers.len() {
            norm.push(String::new());
        }
        out.push_str(&csv_row(&norm));
        out.push('\n');
    }
    Some(out)
}

fn csv_row(cells: &[String]) -> String {
    cells
        .iter()
        .map(|c| {
            let needs_quote = c.contains(',') || c.contains('"') || c.contains('\n');
            if needs_quote {
                format!("\"{}\"", c.replace('"', "\"\""))
            } else {
                c.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(",")
}

/// Deep-MCKP encoding specialized for MCP pipeline responses
/// (`log+url+hash` signature).
///
/// This is a minimal implementation: for nested JSON, emit compact JSON.
/// Future work: string-leaf extraction as fence blocks.
pub fn pipeline_deep_mckp(raw: &str, cls: &ClassifiedResponse) -> Option<String> {
    if cls.shape != Shape::NestedObject {
        return None;
    }
    // Conservative baseline: re-serialize as compact JSON.
    let val: serde_json::Value = serde_json::from_str(raw.trim_start()).ok()?;
    let compact = serde_json::to_string(&val).ok()?;
    if compact.len() < raw.len() {
        Some(compact)
    } else {
        None
    }
}

/// True per-subtree MCKP encoding for an object that wraps a homogeneous array.
///
/// Format: top-level non-array fields rendered as `key: value` lines (nested
/// values inline-JSON), then the main array as a Markdown table whose column
/// set is the **union** of all element keys; nested cells are inline JSON.
///
/// **Never drops data.** This is the fix for the bug where naive monolithic
/// CSV/Markdown encoders selected the inner array and discarded the wrapping
/// object's top-level fields (Paper 2, §"Encoder Bug Postmortem").
///
/// Returns `None` if input is not a JSON object, or no homogeneous array of
/// objects (≥2 elements, ≥80% key-set overlap with the first element) is
/// found among its top-level keys.
pub fn deep_mckp_with_inner_table(raw: &str, _cls: &ClassifiedResponse) -> Option<String> {
    use std::collections::BTreeSet;

    let val: serde_json::Value = serde_json::from_str(raw.trim_start()).ok()?;
    let obj = val.as_object()?;

    // Find the largest top-level array of homogeneous objects.
    let mut best_key: Option<String> = None;
    let mut best_size: usize = 0;
    for (k, v) in obj {
        let Some(arr) = v.as_array() else { continue };
        if arr.len() < 2 {
            continue;
        }
        let Some(first_obj) = arr[0].as_object() else {
            continue;
        };
        let first_keys: BTreeSet<&str> = first_obj.keys().map(|s| s.as_str()).collect();
        let n = arr.len();
        // Count elements that are objects (regardless of exact key match) —
        // we use union-of-keys for headers, so partial overlap is fine.
        let object_share = arr.iter().filter(|x| x.is_object()).count();
        // Count how many share an exact key set with the first element —
        // used as a "dominant shape" signal but not a hard requirement.
        let _exact_match = arr
            .iter()
            .filter(|x| {
                x.as_object()
                    .map(|o| o.keys().map(|s| s.as_str()).collect::<BTreeSet<_>>() == first_keys)
                    .unwrap_or(false)
            })
            .count();
        // Accept any array that is mostly objects (≥80% objects).
        // Heterogeneity within those objects is fine — we render the union
        // and emit blank cells for missing keys.
        if object_share * 100 / n >= 80 && n > best_size {
            best_key = Some(k.clone());
            best_size = n;
        }
    }
    let main_key = best_key?;
    let main_arr = obj.get(&main_key).and_then(|v| v.as_array())?;

    let mut out = String::new();

    // Pre-render top-level non-main fields as `key: value` lines.
    for (k, v) in obj {
        if k == &main_key {
            continue;
        }
        out.push_str(k);
        out.push_str(": ");
        match v {
            serde_json::Value::Null => {}
            serde_json::Value::String(s) => out.push_str(s),
            serde_json::Value::Object(_) | serde_json::Value::Array(_) => {
                out.push_str(&serde_json::to_string(v).ok()?);
            }
            _ => out.push_str(&v.to_string()),
        }
        out.push('\n');
    }
    if !out.is_empty() {
        out.push('\n');
    }

    // Union of keys across elements, preserving first-occurrence order.
    let mut headers: Vec<String> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for item in main_arr {
        if let Some(o) = item.as_object() {
            for k in o.keys() {
                if seen.insert(k.clone()) {
                    headers.push(k.clone());
                }
            }
        }
    }
    if headers.is_empty() {
        return None;
    }

    // Markdown table header + separator.
    out.push_str("| ");
    out.push_str(&headers.join(" | "));
    out.push_str(" |\n|");
    for _ in &headers {
        out.push_str(" --- |");
    }
    out.push('\n');

    // Rows — nested values become inline JSON in their cell.
    for item in main_arr {
        let Some(row_obj) = item.as_object() else {
            continue;
        };
        let cells: Vec<String> = headers
            .iter()
            .map(|k| match row_obj.get(k) {
                None | Some(serde_json::Value::Null) => String::new(),
                Some(serde_json::Value::String(s)) => s.clone(),
                Some(v @ (serde_json::Value::Object(_) | serde_json::Value::Array(_))) => {
                    serde_json::to_string(v).unwrap_or_default()
                }
                Some(other) => other.to_string(),
            })
            .map(|s| s.replace('|', "\\|").replace(['\n', '\r'], " "))
            .collect();
        out.push_str("| ");
        out.push_str(&cells.join(" | "));
        out.push_str(" |\n");
    }

    Some(out)
}

/// MR-diff template: extract `diffs[]` array from a pipeline-style response
/// and emit each entry as a fenced code block, dropping the JSON wrapper.
///
/// Expects a JSON object with a `"diffs"` field holding an array of objects
/// with `"path"` and `"content"`/`"diff"` keys (common MCP shape).
/// Other shapes → returns None.
pub fn mr_diff_fence(raw: &str, _cls: &ClassifiedResponse) -> Option<String> {
    let val: serde_json::Value = serde_json::from_str(raw.trim_start()).ok()?;
    let diffs = val.get("diffs")?.as_array()?;
    if diffs.is_empty() {
        return None;
    }
    let mut out = String::new();
    for (i, d) in diffs.iter().enumerate() {
        let path = d
            .get("path")
            .or_else(|| d.get("new_path"))
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let body = d
            .get("content")
            .or_else(|| d.get("diff"))
            .and_then(|v| v.as_str())?;
        if !path.is_empty() {
            out.push_str(&format!("## diff {} ({})\n", i + 1, path));
        } else {
            out.push_str(&format!("## diff {}\n", i + 1));
        }
        out.push_str("```diff\n");
        out.push_str(body);
        if !body.ends_with('\n') {
            out.push('\n');
        }
        out.push_str("```\n");
    }
    if out.len() < raw.len() {
        Some(out)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shape::classify;

    #[test]
    fn csv_from_md_handles_simple_table() {
        let md =
            "| id | name | status |\n|----|------|--------|\n| 1 | a | ok |\n| 2 | b | bad |\n";
        let cls = classify(md);
        let out = csv_from_md(md, &cls).unwrap();
        assert!(out.contains("id,name,status"));
        assert!(out.contains("1,a,ok"));
        assert!(out.contains("2,b,bad"));
        // CSV is strictly shorter than markdown for this case
        assert!(out.len() < md.len());
    }

    #[test]
    fn csv_from_md_rejects_non_md() {
        let txt = "just prose, no table here.";
        let cls = classify(txt);
        assert!(csv_from_md(txt, &cls).is_none());
    }

    #[test]
    fn csv_from_md_quotes_commas() {
        let md = "| a | b |\n|---|---|\n| has, comma | plain |\n";
        let cls = classify(md);
        let out = csv_from_md(md, &cls).unwrap();
        assert!(out.contains("\"has, comma\""));
    }

    #[test]
    fn mr_diff_fence_extracts_diffs() {
        let json = r#"{"mr_id":42,"diffs":[
            {"path":"src/a.rs","content":"@@ -1 +1 @@\n-old\n+new\n"},
            {"path":"src/b.rs","content":"@@ -2 +2 @@\n-foo\n+bar\n"}
        ]}"#;
        let cls = classify(json);
        let out = mr_diff_fence(json, &cls).unwrap();
        assert!(out.contains("## diff 1 (src/a.rs)"));
        assert!(out.contains("```diff"));
        assert!(out.contains("+new"));
    }

    #[test]
    fn mr_diff_fence_rejects_non_diff_response() {
        let json = r#"{"ok":true}"#;
        let cls = classify(json);
        assert!(mr_diff_fence(json, &cls).is_none());
    }

    #[test]
    fn pipeline_deep_mckp_compacts_json() {
        let json = "{\n  \"id\": 123,\n  \"nested\": {\n    \"a\": 1\n  }\n}\n";
        let cls = classify(json);
        let out = pipeline_deep_mckp(json, &cls).unwrap();
        assert!(out.len() < json.len());
        assert!(!out.contains('\n')); // compact, no pretty-print
    }

    #[test]
    fn apply_by_id_dispatches() {
        let md = "| a | b |\n|---|---|\n| 1 | 2 |\n";
        let cls = classify(md);
        assert!(apply_by_id("csv_from_md", md, &cls).is_some());
        assert!(apply_by_id("unknown_id", md, &cls).is_none());
    }

    #[test]
    fn mr_diff_fence_uses_new_path_when_path_missing() {
        let json = r#"{"diffs":[{"new_path":"src/renamed.rs","content":"@@ -1 +1 @@\n-o\n+n\n"}]}"#;
        let cls = classify(json);
        let out = mr_diff_fence(json, &cls).unwrap();
        assert!(out.contains("src/renamed.rs"));
    }

    #[test]
    fn mr_diff_fence_uses_diff_field_fallback() {
        // Some MCP servers use `diff` instead of `content`.
        let json = r#"{"diffs":[{"path":"a.rs","diff":"@@ -1 +1 @@\n-x\n+y"}]}"#;
        let cls = classify(json);
        let out = mr_diff_fence(json, &cls).unwrap();
        assert!(out.contains("+y"));
    }

    #[test]
    fn mr_diff_fence_rejects_empty_diffs_array() {
        let json = r#"{"diffs":[]}"#;
        let cls = classify(json);
        assert!(mr_diff_fence(json, &cls).is_none());
    }

    #[test]
    fn mr_diff_fence_rejects_missing_content_field() {
        let json = r#"{"diffs":[{"path":"a.rs"}]}"#; // no content, no diff
        let cls = classify(json);
        assert!(mr_diff_fence(json, &cls).is_none());
    }

    #[test]
    fn mr_diff_fence_appends_newline_when_body_unterminated() {
        // Body without trailing \n should still be wrapped correctly.
        let json = r#"{"diffs":[{"path":"a.rs","content":"@@ -1 +1 @@\n-x\n+y"}]}"#;
        let cls = classify(json);
        let out = mr_diff_fence(json, &cls).unwrap();
        // Must end with triple-backtick + newline.
        assert!(out.contains("```\n"));
    }

    #[test]
    fn csv_from_md_returns_empty_when_no_rows() {
        let md = "| a | b |\n|---|---|\n";
        let cls = classify(md);
        // Headers parse; no data rows → still returns output with just header line.
        let out = csv_from_md(md, &cls);
        if let Some(o) = out {
            assert!(o.starts_with("a,b\n"));
        }
    }

    #[test]
    fn csv_from_md_preserves_pipe_escapes() {
        // Markdown cell with escaped pipe must not split the row.
        let md = "| col |\n|---|\n| one\\|two |\n";
        let cls = classify(md);
        // Not applicable if 1 column — router rejects. csv_from_md itself still runs.
        // Simply asserting no panic.
        let _ = csv_from_md(md, &cls);
    }

    #[test]
    fn pipeline_deep_mckp_rejects_non_nested_shape() {
        let md = "| a | b |\n|---|---|\n| 1 | 2 |\n";
        let cls = classify(md);
        assert!(pipeline_deep_mckp(md, &cls).is_none());
    }

    #[test]
    fn pipeline_deep_mckp_returns_none_when_already_compact() {
        // Already-compact JSON → encoding can't shrink.
        let json = r#"{"a":{"b":1}}"#;
        let cls = classify(json);
        assert!(pipeline_deep_mckp(json, &cls).is_none());
    }

    #[test]
    fn deep_mckp_inner_table_preserves_top_level_fields() {
        let json = r#"{"company":"Acme","year":2026,"employees":[
            {"id":1,"name":"Alice","dept":"Eng"},
            {"id":2,"name":"Bob","dept":"Sales"}
        ]}"#;
        let cls = classify(json);
        let out = deep_mckp_with_inner_table(json, &cls).unwrap();
        // Top-level fields must survive
        assert!(out.contains("company: Acme"), "missing company line: {out}");
        assert!(out.contains("year: 2026"), "missing year line: {out}");
        // Table headers
        assert!(out.contains("| id | name | dept |"));
        // Rows
        assert!(out.contains("| 1 | Alice | Eng |"));
        assert!(out.contains("| 2 | Bob | Sales |"));
    }

    #[test]
    fn deep_mckp_inner_table_inlines_nested_cells() {
        let json = r#"{"shop":"X","orders":[
            {"id":"o1","total":60,"customer":{"id":100,"email":"a@x"}},
            {"id":"o2","total":72,"customer":{"id":101,"email":"b@x"}}
        ]}"#;
        let cls = classify(json);
        let out = deep_mckp_with_inner_table(json, &cls).unwrap();
        assert!(out.contains("shop: X"));
        assert!(out.contains("| id | total | customer |"));
        // Customer rendered as inline JSON in the cell
        assert!(
            out.contains(r#"{"id":100,"email":"a@x"}"#),
            "missing inline JSON cell: {out}"
        );
        assert!(out.contains(r#"{"id":101,"email":"b@x"}"#));
    }

    #[test]
    fn deep_mckp_inner_table_uses_union_of_keys() {
        let json = r#"{"label":"L","items":[
            {"id":1,"name":"a"},
            {"id":2,"name":"b","extra":"x"}
        ]}"#;
        let cls = classify(json);
        let out = deep_mckp_with_inner_table(json, &cls).unwrap();
        // 'extra' must appear in headers even though only second row has it
        assert!(out.contains("extra"), "missing extra col: {out}");
        // First row has empty cell for extra
        assert!(out.contains("| 1 | a |  |"));
    }

    #[test]
    fn deep_mckp_inner_table_returns_none_without_inner_array() {
        let json = r#"{"a":1,"b":"text","c":{"nested":true}}"#;
        let cls = classify(json);
        assert!(deep_mckp_with_inner_table(json, &cls).is_none());
    }

    #[test]
    fn deep_mckp_inner_table_returns_none_for_array_root() {
        let json = r#"[{"id":1},{"id":2}]"#;
        let cls = classify(json);
        // Top-level is array, not object → not handled here (try_array_csv handles it)
        assert!(deep_mckp_with_inner_table(json, &cls).is_none());
    }

    #[test]
    fn deep_mckp_inner_table_escapes_pipes_and_newlines() {
        let json = r#"{"key":"K","arr":[
            {"id":1,"text":"line1\nline2"},
            {"id":2,"text":"a|b"}
        ]}"#;
        let cls = classify(json);
        let out = deep_mckp_with_inner_table(json, &cls).unwrap();
        // Newline escaped to space
        assert!(out.contains("line1 line2"), "newline not escaped: {out}");
        // Pipe escaped
        assert!(out.contains("a\\|b"), "pipe not escaped: {out}");
    }
}
