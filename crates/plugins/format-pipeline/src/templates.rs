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
            .and_then(|v| v.as_str());
        let Some(body) = body else {
            return None;
        };
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
        let md = "| id | name | status |\n|----|------|--------|\n| 1 | a | ok |\n| 2 | b | bad |\n";
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
}
