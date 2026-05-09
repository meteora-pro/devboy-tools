//! Structural shape classifier for tool responses.
//!
//! Ports the classifier from `docs/research/scripts/extract_paper2_format_events.py`
//! into Rust. Keep the two in sync so offline analyses and online decisions
//! share the same taxonomy.
//!
//! Shape is detected by a two-step rule:
//! 1. If the content parses as JSON, classify by JSON structure.
//! 2. Otherwise, scan the text for markdown / code / numbered-list patterns.

use crate::telemetry::Shape;
use serde_json::Value;

/// Result of classifying one response.
#[derive(Debug, Clone)]
pub struct ClassifiedResponse {
    pub shape: Shape,
    pub raw_chars: usize,
    pub inner_formats: Vec<InnerFormat>,
    /// For markdown-table: number of columns detected in the header row.
    pub md_n_cols: Option<usize>,
    pub md_n_rows: Option<usize>,
    /// For array_of_objects: number of elements.
    pub n_items: Option<usize>,
    /// For array_of_objects: mean jaccard of key sets across items (0–1).
    pub key_stability: Option<f32>,
    /// For flat_object / nested_object: number of top-level keys.
    pub n_fields: Option<usize>,
    /// For nested_object: max depth of JSON nesting.
    pub depth_max: Option<usize>,
}

/// Categories of inner formats we sniff inside string leaves and prose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InnerFormat {
    Url,
    Log,
    Hash,
    Diff,
    Markdown,
    /// MarkdownTable.
    MarkdownTable,
    /// MarkdownWithCode.
    MarkdownWithCode,
    /// CodeFence.
    CodeFence,
    /// XmlHtml.
    XmlHtml,
    Yaml,
    /// StackTrace.
    StackTrace,
    /// NumberedList.
    NumberedList,
    /// InlineJson.
    InlineJson,
    Prose,
}

impl InnerFormat {
    /// As tag.
    pub fn as_tag(&self) -> &'static str {
        match self {
            Self::Url => "url",
            Self::Log => "log",
            Self::Hash => "hash",
            Self::Diff => "diff",
            Self::Markdown => "md",
            Self::MarkdownTable => "md_table",
            Self::MarkdownWithCode => "md_with_code",
            Self::CodeFence => "code_fence",
            Self::XmlHtml => "xml_html",
            Self::Yaml => "yaml",
            Self::StackTrace => "stack_trace",
            Self::NumberedList => "numbered_list",
            Self::InlineJson => "inline_json",
            Self::Prose => "prose",
        }
    }
}

// ─── TEXT PATTERN DETECTORS ──────────────────────────────────────────────

fn has_md_table(text: &str) -> Option<(usize, usize)> {
    // Minimum valid markdown table: `|a|b|` header + `|---|---|` separator.
    let lines: Vec<&str> = text.lines().collect();
    for (i, line) in lines.iter().enumerate() {
        if !line.trim_start().starts_with('|') || !line.trim_end().ends_with('|') {
            continue;
        }
        // Check next line is a separator.
        let next = lines.get(i + 1)?;
        let n_trim = next.trim();
        if !n_trim.starts_with('|') {
            continue;
        }
        let chars_ok = n_trim
            .chars()
            .all(|c| c == '|' || c == '-' || c == ':' || c.is_whitespace());
        if !chars_ok {
            continue;
        }
        // Count columns by pipe segments (trimmed outer).
        let n_cols = line
            .trim_matches('|')
            .split('|')
            .filter(|c| !c.trim().is_empty() || !c.is_empty())
            .count();
        // Count data rows (lines after separator that begin with |).
        let mut n_rows = 0;
        for l in &lines[i + 2..] {
            if l.trim_start().starts_with('|') {
                n_rows += 1;
            } else if l.trim().is_empty() {
                break;
            }
        }
        return Some((n_cols, n_rows));
    }
    None
}

fn has_code_fence(text: &str) -> bool {
    text.lines()
        .filter(|l| l.trim_start().starts_with("```"))
        .count()
        >= 2
}

fn has_numbered_list(text: &str) -> bool {
    // Matches lines like `   1→`, `  42→` — the Read tool's line-number prefix.
    text.lines()
        .filter(|l| {
            let mut chars = l.chars();
            let mut seen_digit = false;
            for c in chars.by_ref() {
                if c == ' ' {
                    continue;
                }
                if c.is_ascii_digit() {
                    seen_digit = true;
                    continue;
                }
                return seen_digit && c == '→';
            }
            false
        })
        .count()
        >= 3
}

fn has_bullet_list(text: &str) -> bool {
    text.lines()
        .filter(|l| {
            let t = l.trim_start();
            (t.starts_with("- ") || t.starts_with("* ")) && !t.starts_with("--")
        })
        .count()
        >= 3
}

/// Very rough URL detection — just looks for `http://` or `https://`.
fn count_urls(text: &str) -> usize {
    let mut n = 0;
    let bytes = text.as_bytes();
    let mut i = 0;
    while i + 7 <= bytes.len() {
        let w = &bytes[i..i + 7];
        if w == b"http://" || (i + 8 <= bytes.len() && &bytes[i..i + 8] == b"https://") {
            n += 1;
            i += 8;
        } else {
            i += 1;
        }
    }
    n
}

/// Timestamp patterns like `2026-04-24 18:29:00` or `2026-04-24T18:29:00`.
fn count_timestamps(text: &str) -> usize {
    let bytes = text.as_bytes();
    let mut n = 0;
    let mut i = 0;
    while i + 19 <= bytes.len() {
        if bytes[i].is_ascii_digit()
            && bytes[i + 1].is_ascii_digit()
            && bytes[i + 2].is_ascii_digit()
            && bytes[i + 3].is_ascii_digit()
            && bytes[i + 4] == b'-'
            && bytes[i + 5].is_ascii_digit()
            && bytes[i + 6].is_ascii_digit()
            && bytes[i + 7] == b'-'
            && bytes[i + 8].is_ascii_digit()
            && bytes[i + 9].is_ascii_digit()
            && (bytes[i + 10] == b' ' || bytes[i + 10] == b'T')
            && bytes[i + 11].is_ascii_digit()
            && bytes[i + 12].is_ascii_digit()
            && bytes[i + 13] == b':'
        {
            n += 1;
            i += 19;
        } else {
            i += 1;
        }
    }
    n
}

/// Git-style hex hashes (7–40 hex chars, word-bounded).
fn count_hashes(text: &str) -> usize {
    let mut n = 0;
    let mut run = 0;
    for c in text.chars() {
        if c.is_ascii_hexdigit() {
            run += 1;
        } else {
            if (7..=40).contains(&run) {
                n += 1;
            }
            run = 0;
        }
    }
    if (7..=40).contains(&run) {
        n += 1;
    }
    n
}

/// Unified-diff markers (`@@ -1,5 +1,6 @@`).
fn has_diff(text: &str) -> bool {
    text.lines().any(|l| {
        let t = l.trim_start();
        t.starts_with("@@ ") && t.contains(" @@")
    }) || text.contains("diff --git")
}

fn has_stack_trace(text: &str) -> bool {
    text.contains("Traceback (most recent call last):") || text.contains("\n    at ")
}

// ─── JSON CLASSIFIER ────────────────────────────────────────────────────

fn classify_json(val: &Value) -> (Shape, JsonDetails) {
    let mut details = JsonDetails::default();
    match val {
        Value::Array(items) => {
            details.n_items = Some(items.len());
            if items.is_empty() {
                return (Shape::Empty, details);
            }
            // Determine majority variant.
            let all_objects = items.iter().all(|v| v.is_object());
            if all_objects {
                details.key_stability = Some(compute_key_stability(items));
                details.has_nested_values = items.iter().take(20).any(|v| {
                    if let Value::Object(m) = v {
                        m.values().any(|vv| vv.is_object() || vv.is_array())
                    } else {
                        false
                    }
                });
                (Shape::ArrayOfObjects, details)
            } else if items
                .iter()
                .all(|v| v.is_string() || v.is_number() || v.is_boolean() || v.is_null())
            {
                (Shape::ArrayOfPrimitives, details)
            } else {
                (Shape::NestedObject, details) // heterogeneous array → treat as nested
            }
        }
        Value::Object(m) => {
            details.n_fields = Some(m.len());
            if m.is_empty() {
                return (Shape::Empty, details);
            }
            let any_nested = m.values().any(|v| v.is_object() || v.is_array());
            if any_nested {
                details.depth_max = Some(json_depth(val));
                (Shape::NestedObject, details)
            } else {
                (Shape::FlatObject, details)
            }
        }
        _ => (Shape::Unknown, details),
    }
}

fn json_depth(val: &Value) -> usize {
    match val {
        Value::Object(m) => 1 + m.values().map(json_depth).max().unwrap_or(0),
        Value::Array(a) => 1 + a.iter().map(json_depth).max().unwrap_or(0),
        _ => 0,
    }
}

fn compute_key_stability(items: &[Value]) -> f32 {
    use std::collections::HashSet;
    let sets: Vec<HashSet<String>> = items
        .iter()
        .take(20)
        .filter_map(|v| v.as_object().map(|o| o.keys().cloned().collect()))
        .collect();
    if sets.len() < 2 {
        return 1.0;
    }
    let first = &sets[0];
    let mut jac = Vec::with_capacity(sets.len() - 1);
    for s in &sets[1..] {
        let union: HashSet<_> = first.union(s).cloned().collect();
        let inter: HashSet<_> = first.intersection(s).cloned().collect();
        if union.is_empty() {
            jac.push(1.0);
        } else {
            jac.push(inter.len() as f32 / union.len() as f32);
        }
    }

    jac.iter().sum::<f32>() / jac.len() as f32
}

#[derive(Default)]
struct JsonDetails {
    n_items: Option<usize>,
    n_fields: Option<usize>,
    depth_max: Option<usize>,
    key_stability: Option<f32>,
    has_nested_values: bool,
}

// ─── PUBLIC ENTRY ───────────────────────────────────────────────────────

/// Classify a response body. Never panics; always returns a Shape
/// (Unknown if nothing matches).
pub fn classify(content: &str) -> ClassifiedResponse {
    let raw_chars = content.len();

    // Try JSON first.
    let trimmed = content.trim_start();
    if (trimmed.starts_with('{') || trimmed.starts_with('['))
        && let Ok(val) = serde_json::from_str::<Value>(trimmed)
    {
        let (shape, details) = classify_json(&val);
        let inner = scan_inner_formats_in_json(&val);
        return ClassifiedResponse {
            shape,
            raw_chars,
            inner_formats: inner,
            md_n_cols: None,
            md_n_rows: None,
            n_items: details.n_items,
            key_stability: details.key_stability,
            n_fields: details.n_fields,
            depth_max: details.depth_max,
        };
    }

    // Text-level classification.
    if let Some((cols, rows)) = has_md_table(content) {
        return ClassifiedResponse {
            shape: Shape::MarkdownTable,
            raw_chars,
            inner_formats: text_inner_formats(content),
            md_n_cols: Some(cols),
            md_n_rows: Some(rows),
            n_items: None,
            key_stability: None,
            n_fields: None,
            depth_max: None,
        };
    }
    if has_code_fence(content) {
        return ClassifiedResponse {
            shape: Shape::CodeBlock,
            raw_chars,
            inner_formats: text_inner_formats(content),
            md_n_cols: None,
            md_n_rows: None,
            n_items: None,
            key_stability: None,
            n_fields: None,
            depth_max: None,
        };
    }
    if has_numbered_list(content) {
        return ClassifiedResponse {
            shape: Shape::NumberedList,
            raw_chars,
            inner_formats: vec![],
            md_n_cols: None,
            md_n_rows: None,
            n_items: None,
            key_stability: None,
            n_fields: None,
            depth_max: None,
        };
    }
    if has_bullet_list(content) {
        return ClassifiedResponse {
            shape: Shape::BulletList,
            raw_chars,
            inner_formats: text_inner_formats(content),
            md_n_cols: None,
            md_n_rows: None,
            n_items: None,
            key_stability: None,
            n_fields: None,
            depth_max: None,
        };
    }
    ClassifiedResponse {
        shape: Shape::Prose,
        raw_chars,
        inner_formats: text_inner_formats(content),
        md_n_cols: None,
        md_n_rows: None,
        n_items: None,
        key_stability: None,
        n_fields: None,
        depth_max: None,
    }
}

fn text_inner_formats(text: &str) -> Vec<InnerFormat> {
    let mut out = Vec::new();
    if count_urls(text) > 0 {
        out.push(InnerFormat::Url);
    }
    if count_timestamps(text) > 0 {
        out.push(InnerFormat::Log);
    }
    if count_hashes(text) > 0 {
        out.push(InnerFormat::Hash);
    }
    if has_diff(text) {
        out.push(InnerFormat::Diff);
    }
    if has_stack_trace(text) {
        out.push(InnerFormat::StackTrace);
    }
    out
}

fn scan_inner_formats_in_json(val: &Value) -> Vec<InnerFormat> {
    use std::collections::HashSet;
    let mut seen: HashSet<&'static str> = HashSet::new();
    walk_json_strings(val, &mut seen, 0);
    let mut out = Vec::new();
    for tag in [
        "url",
        "log",
        "hash",
        "diff",
        "md",
        "md_table",
        "xml_html",
        "yaml",
        "stack_trace",
        "numbered_list",
        "prose",
    ] {
        if seen.contains(tag) {
            out.push(match tag {
                "url" => InnerFormat::Url,
                "log" => InnerFormat::Log,
                "hash" => InnerFormat::Hash,
                "diff" => InnerFormat::Diff,
                "md" => InnerFormat::Markdown,
                "md_table" => InnerFormat::MarkdownTable,
                "xml_html" => InnerFormat::XmlHtml,
                "yaml" => InnerFormat::Yaml,
                "stack_trace" => InnerFormat::StackTrace,
                "numbered_list" => InnerFormat::NumberedList,
                "prose" => InnerFormat::Prose,
                _ => continue,
            });
        }
    }
    out
}

fn walk_json_strings(
    val: &Value,
    seen: &mut std::collections::HashSet<&'static str>,
    depth: usize,
) {
    if depth > 5 {
        return;
    }
    match val {
        Value::String(s) => {
            if s.len() < 8 {
                return;
            }
            if count_urls(s) > 0 {
                seen.insert("url");
            }
            if count_timestamps(s) > 0 {
                seen.insert("log");
            }
            if count_hashes(s) > 0 {
                seen.insert("hash");
            }
            if has_diff(s) {
                seen.insert("diff");
            }
            if has_md_table(s).is_some() {
                seen.insert("md_table");
            }
            if has_stack_trace(s) {
                seen.insert("stack_trace");
            }
        }
        Value::Array(items) => {
            for v in items.iter().take(100) {
                walk_json_strings(v, seen, depth + 1);
            }
        }
        Value::Object(m) => {
            for v in m.values().take(200) {
                walk_json_strings(v, seen, depth + 1);
            }
        }
        _ => {}
    }
}

// ─── TESTS ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_json_array_of_objects() {
        let text = r#"[{"id":1,"name":"a"},{"id":2,"name":"b"}]"#;
        let c = classify(text);
        assert_eq!(c.shape, Shape::ArrayOfObjects);
        assert_eq!(c.n_items, Some(2));
        assert!(c.key_stability.unwrap() > 0.99);
    }

    #[test]
    fn classifies_flat_object() {
        let text = r#"{"a":1,"b":"text","c":true}"#;
        let c = classify(text);
        assert_eq!(c.shape, Shape::FlatObject);
        assert_eq!(c.n_fields, Some(3));
    }

    #[test]
    fn classifies_nested_object() {
        let text = r#"{"a":{"b":{"c":1}}}"#;
        let c = classify(text);
        assert_eq!(c.shape, Shape::NestedObject);
        assert!(c.depth_max.unwrap() >= 3);
    }

    #[test]
    fn classifies_markdown_table() {
        let text = "| id | name |\n|----|------|\n| 1 | Alice |\n| 2 | Bob |\n";
        let c = classify(text);
        assert_eq!(c.shape, Shape::MarkdownTable);
        assert_eq!(c.md_n_cols, Some(2));
        assert_eq!(c.md_n_rows, Some(2));
    }

    #[test]
    fn classifies_code_block() {
        let text = "Some docs.\n```python\ndef foo():\n    return 1\n```\n";
        let c = classify(text);
        assert_eq!(c.shape, Shape::CodeBlock);
    }

    #[test]
    fn classifies_numbered_list_file_read() {
        let text = "   1→use chrono::DateTime;\n   2→use serde::Deserialize;\n   3→pub struct X;\n";
        let c = classify(text);
        assert_eq!(c.shape, Shape::NumberedList);
    }

    #[test]
    fn classifies_prose_with_url() {
        let text = "Here is a URL: https://example.com/foo and some text.";
        let c = classify(text);
        assert_eq!(c.shape, Shape::Prose);
        assert!(c.inner_formats.contains(&InnerFormat::Url));
    }

    #[test]
    fn detects_log_and_hash_in_json_strings() {
        let text = r#"{"commit":"abc1234def","time":"2026-04-24 18:30:00","url":"https://x.y/z"}"#;
        let c = classify(text);
        assert_eq!(c.shape, Shape::FlatObject);
        assert!(c.inner_formats.contains(&InnerFormat::Log));
        assert!(c.inner_formats.contains(&InnerFormat::Hash));
        assert!(c.inner_formats.contains(&InnerFormat::Url));
    }

    #[test]
    fn detects_diff() {
        let text = "--- a/foo\n+++ b/foo\n@@ -1,3 +1,3 @@\n-old\n+new\n line\n";
        let c = classify(text);
        assert!(c.inner_formats.contains(&InnerFormat::Diff));
    }

    #[test]
    fn empty_array_is_empty() {
        let c = classify("[]");
        assert_eq!(c.shape, Shape::Empty);
    }

    #[test]
    fn empty_object_is_empty() {
        let c = classify("{}");
        assert_eq!(c.shape, Shape::Empty);
    }

    #[test]
    fn classifies_array_of_primitives() {
        let c = classify("[1, 2, 3, 4, 5]");
        assert_eq!(c.shape, Shape::ArrayOfPrimitives);
        assert_eq!(c.n_items, Some(5));
    }

    #[test]
    fn classifies_heterogeneous_array_as_nested() {
        // Mixed types → fallthrough to NestedObject treatment.
        let c = classify(r#"[1, "two", {"three": 3}]"#);
        assert_eq!(c.shape, Shape::NestedObject);
    }

    #[test]
    fn classifies_bullet_list() {
        let text = "Items:\n- one\n- two\n- three\n";
        let c = classify(text);
        assert_eq!(c.shape, Shape::BulletList);
    }

    #[test]
    fn classifies_plain_prose_fallback() {
        let c = classify("Just one sentence, no structure.");
        assert_eq!(c.shape, Shape::Prose);
    }

    #[test]
    fn detects_python_traceback_in_prose() {
        let text = "Traceback (most recent call last):\n  File \"x.py\", line 1, in <module>\n    raise ValueError(\"bad\")\nValueError: bad\n";
        let c = classify(text);
        assert!(c.inner_formats.contains(&InnerFormat::StackTrace));
    }

    #[test]
    fn detects_js_style_stack_trace() {
        let text =
            "Error occurred\n    at Object.<anonymous> (/foo.js:1:1)\n    at Module._compile\n";
        let c = classify(text);
        assert!(c.inner_formats.contains(&InnerFormat::StackTrace));
    }

    #[test]
    fn detects_git_diff_header() {
        let text = "diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1 +1 @@\n-a\n+b\n";
        let c = classify(text);
        assert!(c.inner_formats.contains(&InnerFormat::Diff));
    }

    #[test]
    fn classifies_nested_object_with_diff_inside() {
        let text = r#"{"mr_id":42,"diffs":"@@ -1,3 +1,3 @@\n-old\n+new"}"#;
        let c = classify(text);
        assert_eq!(c.shape, Shape::FlatObject);
        assert!(c.inner_formats.contains(&InnerFormat::Diff));
    }

    #[test]
    fn detects_md_table_inside_json_string() {
        let text = r#"{"body":"| a | b |\n|---|---|\n| 1 | 2 |\n"}"#;
        let c = classify(text);
        assert!(c.inner_formats.contains(&InnerFormat::MarkdownTable));
    }

    #[test]
    fn inner_format_as_tag_covers_all_variants() {
        // Guards against a new InnerFormat variant forgetting a tag.
        let variants = [
            InnerFormat::Url,
            InnerFormat::Log,
            InnerFormat::Hash,
            InnerFormat::Diff,
            InnerFormat::Markdown,
            InnerFormat::MarkdownTable,
            InnerFormat::MarkdownWithCode,
            InnerFormat::CodeFence,
            InnerFormat::XmlHtml,
            InnerFormat::Yaml,
            InnerFormat::StackTrace,
            InnerFormat::NumberedList,
            InnerFormat::InlineJson,
            InnerFormat::Prose,
        ];
        for v in &variants {
            assert!(!v.as_tag().is_empty(), "missing tag for {v:?}");
        }
    }

    #[test]
    fn array_of_objects_key_stability_detects_drift() {
        // Two objects with zero key overlap → stability close to 0.
        let text = r#"[{"a":1,"b":2}, {"c":3,"d":4}]"#;
        let c = classify(text);
        assert_eq!(c.shape, Shape::ArrayOfObjects);
        assert!(
            c.key_stability.unwrap() < 0.1,
            "expected low stability, got {:?}",
            c.key_stability
        );
    }

    #[test]
    fn malformed_json_falls_through_to_text_classifier() {
        let text = "{ malformed, not json at all";
        let c = classify(text);
        // Neither object nor array → classified as prose / text fallback.
        assert!(matches!(
            c.shape,
            Shape::Prose | Shape::BulletList | Shape::CodeBlock | Shape::MarkdownTable
        ));
    }

    #[test]
    fn json_inside_string_opens_up_recursion() {
        // Very nested JSON containing long URLs — exercise walk_json_strings recursion.
        let deep = r#"{"a":{"b":{"c":{"d":{"e":"https://nested.example/path/here"}}}}}"#;
        let c = classify(deep);
        assert_eq!(c.shape, Shape::NestedObject);
        assert!(c.inner_formats.contains(&InnerFormat::Url));
    }
}
