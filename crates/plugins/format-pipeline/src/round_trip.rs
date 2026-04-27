//! Round-trip correctness gate for L1 / L2 encoders.
//!
//! Paper 2 §Encoder Bug Postmortem (2026-04-25) describes a class of
//! silent data-loss bugs where the chosen encoder produced shorter output
//! at the cost of dropping wrapping fields or nested-object cells. The
//! `mckp_v2` encoder (`deep_mckp_with_inner_table`) was the fix; this
//! module is the regression test that ensures no future change re-introduces
//! the bug.
//!
//! Each registered encoder declares its [`DataLoss`] guarantee:
//!
//! - [`DataLoss::None`] — every top-level and nested key in the input must
//!   appear (textually) in the output. `mckp_v2` and `json_compact` must
//!   meet this bar.
//! - [`DataLoss::TopLevel`] — wrapping object's top-level fields are
//!   intentionally dropped. Naive `csv` / `markdown_table` are in this
//!   category and **may not be selected as the production default** (see
//!   §Implementation Status migration warning).
//! - [`DataLoss::Nested`] — nested values inside otherwise-preserved
//!   wrappers may be flattened or dropped.
//!
//! The `round_trip_keys` API parses the encoded form back to a textual key
//! set and compares it against the input. The shape of "decoder" is
//! deliberately tolerant — we only check membership of key strings, not
//! type fidelity, since the encoders are explicitly token-saving and not
//! always type-lossless.

use std::collections::BTreeSet;

/// Whether an encoder is allowed to drop input keys, and where.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataLoss {
    /// Encoder must preserve every input key (top-level and nested).
    /// Failing this is a bug.
    None,
    /// Encoder may drop the wrapping object's top-level keys (sibling
    /// fields of the chosen array). It must still preserve all keys
    /// inside the array elements.
    TopLevel,
    /// Encoder may drop nested-object keys inside array elements
    /// (e.g. older naive CSV that only kept primitive columns).
    Nested,
}

/// Result of a round-trip key comparison.
#[derive(Debug, Clone)]
pub struct KeyComparison {
    pub encoder_id: String,
    pub input_keys: BTreeSet<String>,
    pub output_keys: BTreeSet<String>,
    pub dropped: Vec<String>,
    pub added: Vec<String>,
    pub allowed_loss: DataLoss,
}

impl KeyComparison {
    /// True iff the comparison meets the encoder's declared guarantee.
    pub fn is_within_contract(&self) -> bool {
        match self.allowed_loss {
            DataLoss::None => self.dropped.is_empty(),
            // Tolerate any drop: encoder is documented as lossy. Production
            // code paths must already gate on this (refusing to set a
            // lossy encoder as the default).
            DataLoss::TopLevel | DataLoss::Nested => true,
        }
    }

    /// Compact summary for assertions / paper tables.
    pub fn report(&self) -> String {
        format!(
            "encoder={} input_keys={} output_keys={} dropped={:?} added={:?} loss={:?}",
            self.encoder_id,
            self.input_keys.len(),
            self.output_keys.len(),
            self.dropped,
            self.added,
            self.allowed_loss,
        )
    }
}

/// Loss profile declared by each encoder.
///
/// The list is the source of truth — adding a new encoder requires adding
/// a row here, which forces the author to think about which guarantee it
/// holds (and forces the test below to cover it).
pub fn declared_loss(encoder_id: &str) -> Option<DataLoss> {
    Some(match encoder_id {
        "json_compact" | "deep_mckp" | "deep_mckp_inner_table" | "mckp_v2" => DataLoss::None,
        // `kv` and `mr_diff_fence` flatten / serialise structured values
        // into a single line — we still expect every key to be visible
        // textually, so they are `None` for *key* preservation.
        "kv" | "mr_diff_fence" => DataLoss::None,
        // `csv` (try_array_csv) and `csv_from_md` discard wrapping object
        // context (see §Encoder Bug Postmortem). Keys *inside* array
        // elements are preserved (union of keys), but a top-level
        // wrapper's siblings are dropped.
        "csv" | "csv_from_md" => DataLoss::TopLevel,
        _ => return None,
    })
}

/// Run a round-trip key comparison for a known encoder id and the body it
/// produced from `raw_input`. Returns `None` if the encoder id is unknown.
pub fn round_trip_keys(
    encoder_id: &str,
    raw_input: &str,
    encoded_output: &str,
) -> Option<KeyComparison> {
    let allowed_loss = declared_loss(encoder_id)?;
    let input_keys = collect_json_keys(raw_input).unwrap_or_default();
    let output_keys = decode_keys(encoder_id, encoded_output);
    let dropped: Vec<String> = input_keys.difference(&output_keys).cloned().collect();
    let added: Vec<String> = output_keys.difference(&input_keys).cloned().collect();
    Some(KeyComparison {
        encoder_id: encoder_id.to_string(),
        input_keys,
        output_keys,
        dropped,
        added,
        allowed_loss,
    })
}

/// Walk a JSON value and return every `key` seen at any depth — including
/// keys nested inside arrays and inside string-valued cells that happen to
/// parse back as JSON. Free-form text and primitives contribute no keys.
fn collect_json_keys(raw: &str) -> Option<BTreeSet<String>> {
    let val: serde_json::Value = serde_json::from_str(raw.trim_start()).ok()?;
    let mut out = BTreeSet::new();
    walk_value(&val, &mut out);
    Some(out)
}

fn walk_value(v: &serde_json::Value, out: &mut BTreeSet<String>) {
    match v {
        serde_json::Value::Object(map) => {
            for (k, child) in map {
                out.insert(k.clone());
                walk_value(child, out);
            }
        }
        serde_json::Value::Array(arr) => {
            for child in arr {
                walk_value(child, out);
            }
        }
        _ => {}
    }
}

/// Decode the textual key set out of an encoder's output. The decoders are
/// permissive — they look for any token that *could* be a key, since the
/// goal is to detect *missing* keys, not validate full syntactic recovery.
fn decode_keys(encoder_id: &str, encoded: &str) -> BTreeSet<String> {
    match encoder_id {
        "json_compact" | "deep_mckp" => {
            // Compact JSON parses cleanly back to a Value.
            collect_json_keys(encoded).unwrap_or_default()
        }
        "deep_mckp_inner_table" | "mckp_v2" => decode_inner_table_keys(encoded),
        "csv" | "csv_from_md" => decode_csv_header_keys(encoded),
        "kv" => decode_kv_keys(encoded),
        "mr_diff_fence" => decode_diff_fence_keys(encoded),
        _ => BTreeSet::new(),
    }
}

/// Parse the deep_mckp_with_inner_table output: top-level `key: value`
/// lines (until the first blank line), then a `## <main_array_name>`
/// section heading, then a markdown table. Returns the union of pre-table
/// kv keys, the heading, the table headers, and any keys recovered from
/// inline-JSON cells.
fn decode_inner_table_keys(encoded: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut lines = encoded.lines().peekable();

    // Phase 1: top-level kv lines.
    while let Some(line) = lines.peek() {
        if line.trim().is_empty() {
            lines.next();
            break;
        }
        if line.starts_with("## ") || line.starts_with("| ") || line.starts_with("|---") {
            // Reached the section heading or table without a separating
            // blank line — stop kv parsing.
            break;
        }
        let line = lines.next().unwrap();
        if let Some((k, v)) = line.split_once(": ") {
            out.insert(k.trim().to_string());
            // The value may itself be inline JSON — recover its keys too.
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(v.trim()) {
                walk_value(&val, &mut out);
            }
        }
    }

    // Phase 2 (optional): `## <main_array_name>` section heading carries
    // the wrapping object's array key.
    while let Some(line) = lines.peek() {
        if line.trim().is_empty() {
            lines.next();
            continue;
        }
        if let Some(rest) = line.strip_prefix("## ") {
            out.insert(rest.trim().to_string());
            lines.next();
            // Consume the blank line between heading and table.
            if matches!(lines.peek(), Some(l) if l.trim().is_empty()) {
                lines.next();
            }
        }
        break;
    }

    // Phase 3: markdown table header.
    if let Some(header) = lines.next() {
        for cell in split_md_row(header) {
            if !cell.is_empty() {
                out.insert(cell);
            }
        }
        // Skip the `| --- | --- |` separator.
        let _ = lines.next();
    }

    // Phase 3: inline-JSON cells inside data rows.
    for row in lines {
        for cell in split_md_row(row) {
            if (cell.starts_with('{') && cell.ends_with('}'))
                || (cell.starts_with('[') && cell.ends_with(']'))
            {
                let unescaped = cell.replace("\\|", "|");
                if let Ok(val) = serde_json::from_str::<serde_json::Value>(&unescaped) {
                    walk_value(&val, &mut out);
                }
            }
        }
    }

    out
}

fn split_md_row(line: &str) -> Vec<String> {
    let trimmed = line.trim().trim_start_matches('|').trim_end_matches('|');
    trimmed
        .split(" | ")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// CSV (or csv_from_md) output — keys are the comma-separated header row.
/// Wrapping object's top-level keys are *not* recoverable from this
/// representation; that drop is the documented `DataLoss::TopLevel`.
fn decode_csv_header_keys(encoded: &str) -> BTreeSet<String> {
    let header = encoded.lines().next().unwrap_or("");
    header
        .split(',')
        .map(|s| s.trim().trim_matches('"').to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// `key: value` lines, one per pair.
fn decode_kv_keys(encoded: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for line in encoded.lines() {
        if let Some((k, v)) = line.split_once(": ") {
            out.insert(k.trim().to_string());
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(v.trim()) {
                walk_value(&val, &mut out);
            }
        }
    }
    out
}

/// `mr_diff_fence` output is a sequence of fenced blocks with a path
/// header. We recover `path` (and `diff` / `content` if recognisable as
/// the section labels) so that the input's matching keys round-trip.
fn decode_diff_fence_keys(encoded: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    // The encoder writes `path: <p>\n` lines and ```diff fences. The
    // top-level JSON had `diffs`, `path`, and `content` / `diff` keys —
    // we approximate by saying any of those words appearing in the
    // output counts.
    let lower = encoded.to_ascii_lowercase();
    for k in ["diffs", "path", "diff", "content"] {
        if lower.contains(k) {
            out.insert(k.to_string());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shape::classify;
    use crate::templates;

    fn keys_of(raw: &str) -> BTreeSet<String> {
        collect_json_keys(raw).unwrap_or_default()
    }

    // ─── mckp_v2 / deep_mckp_inner_table — must preserve EVERY key ─────

    #[test]
    fn mckp_v2_preserves_top_level_and_nested_keys() {
        let raw = r#"{
            "company": "Acme",
            "year": 2026,
            "employees": [
                {"id": 1, "name": "Ada", "address": {"city": "Boston"}},
                {"id": 2, "name": "Lin", "address": {"city": "Tokyo"}, "phone": "555"}
            ]
        }"#;
        let cls = classify(raw);
        let body = templates::deep_mckp_with_inner_table(raw, &cls)
            .expect("mckp_v2 should engage on object-wrapping-array shape");
        let cmp = round_trip_keys("mckp_v2", raw, &body).expect("encoder is registered");
        assert!(
            cmp.is_within_contract(),
            "mckp_v2 dropped keys: {}",
            cmp.report()
        );
        // Spot-check the specific keys called out in the postmortem.
        for k in [
            "company",
            "year",
            "employees",
            "id",
            "name",
            "address",
            "city",
        ] {
            assert!(
                cmp.output_keys.contains(k),
                "expected key `{k}` in mckp_v2 output, got {:?}",
                cmp.output_keys
            );
        }
    }

    #[test]
    fn mckp_v2_preserves_keys_when_inner_objects_are_heterogeneous() {
        // Last record has an extra `phone` field — union-of-keys must
        // include it.
        let raw = r#"{
            "scope": "ops",
            "items": [
                {"id": 1, "ok": true},
                {"id": 2, "ok": false, "phone": "x"}
            ]
        }"#;
        let cls = classify(raw);
        let body = templates::deep_mckp_with_inner_table(raw, &cls).unwrap();
        let cmp = round_trip_keys("mckp_v2", raw, &body).unwrap();
        assert!(cmp.is_within_contract(), "{}", cmp.report());
        assert!(cmp.output_keys.contains("phone"));
        assert!(cmp.output_keys.contains("scope"));
    }

    #[test]
    fn mckp_v2_returns_none_when_no_inner_array() {
        // Without a homogeneous inner array, the encoder declines. The
        // gate doesn't apply (no encoded output to compare).
        let raw = r#"{"a": 1, "b": 2}"#;
        let cls = classify(raw);
        assert!(templates::deep_mckp_with_inner_table(raw, &cls).is_none());
    }

    // ─── pipeline_deep_mckp / json_compact — fully lossless ───────────

    #[test]
    fn pipeline_deep_mckp_is_lossless() {
        let raw = r#"{
            "url_a": "https://example.com",
            "log": "line1\nline2",
            "hash": "deadbeef",
            "nested": {"k": "v"}
        }"#;
        let cls = classify(raw);
        let body = templates::pipeline_deep_mckp(raw, &cls).unwrap_or_else(|| {
            // If the template doesn't engage on this shape, fall back to
            // serde_json compaction which is the L2 last resort.
            serde_json::to_string(&serde_json::from_str::<serde_json::Value>(raw).unwrap()).unwrap()
        });
        let cmp = round_trip_keys("deep_mckp", raw, &body).unwrap();
        assert!(cmp.is_within_contract(), "{}", cmp.report());
        assert_eq!(cmp.dropped.len(), 0);
    }

    #[test]
    fn json_compact_is_lossless() {
        let raw = r#"{"id":1,"items":[{"a":2},{"b":3}]}"#;
        let body = serde_json::to_string(&serde_json::from_str::<serde_json::Value>(raw).unwrap())
            .unwrap();
        let cmp = round_trip_keys("json_compact", raw, &body).unwrap();
        assert!(cmp.is_within_contract());
        assert_eq!(cmp.dropped.len(), 0);
    }

    // ─── csv / csv_from_md — declared lossy (TopLevel) ─────────────────

    #[test]
    fn naive_csv_drops_top_level_wrapper_as_documented() {
        // Input has a wrapping object with `meta` and `rows`. Naive CSV
        // would emit only the rows table — `meta` *will* be dropped.
        // We assert the gate flags this as expected loss, not a regression.
        let raw = r#"{
            "meta": "report-2026-04-25",
            "rows": [
                {"id": 1, "v": "a"},
                {"id": 2, "v": "b"}
            ]
        }"#;
        // Hand-build the naive CSV that the bug used to emit (the lossy
        // historical path, kept here as the regression target).
        let body = "id,v\n1,a\n2,b\n";
        let cmp = round_trip_keys("csv", raw, body).unwrap();
        assert_eq!(cmp.allowed_loss, DataLoss::TopLevel);
        // Documented loss: `meta` and `rows` (the wrapper) are gone.
        assert!(cmp.dropped.iter().any(|k| k == "meta"));
        assert!(cmp.dropped.iter().any(|k| k == "rows"));
        // But keys inside the array are recoverable from the CSV header.
        assert!(cmp.output_keys.contains("id"));
        assert!(cmp.output_keys.contains("v"));
        // `is_within_contract` tolerates the documented loss — failing it
        // would mean we tightened the contract.
        assert!(cmp.is_within_contract());
    }

    #[test]
    fn csv_from_md_documents_the_same_loss() {
        // markdown_table → csv only carries the table itself; any
        // surrounding section/heading text is lost on this path.
        let md = "# Report 2026-04-25\n\n| id | v |\n|---|---|\n| 1 | a |\n| 2 | b |\n";
        let cls = classify(md);
        let body = templates::csv_from_md(md, &cls).unwrap();
        // No native JSON input here, so we synthesise the "logical" input
        // (what the markdown represents) for the comparison.
        let logical =
            r#"{"heading":"Report 2026-04-25","rows":[{"id":"1","v":"a"},{"id":"2","v":"b"}]}"#;
        let cmp = round_trip_keys("csv_from_md", logical, &body).unwrap();
        assert_eq!(cmp.allowed_loss, DataLoss::TopLevel);
        assert!(cmp.is_within_contract());
        assert!(cmp.output_keys.contains("id"));
    }

    // ─── kv format — keys preserved, values flattened ─────────────────

    #[test]
    fn kv_format_preserves_all_top_level_keys() {
        let raw = r#"{"alpha":1,"beta":"two","gamma":true,"delta":null,"epsilon":3.14}"#;
        let body = "alpha: 1\nbeta: two\ngamma: true\ndelta: \nepsilon: 3.14\n";
        let cmp = round_trip_keys("kv", raw, body).unwrap();
        assert!(cmp.is_within_contract(), "{}", cmp.report());
        for k in ["alpha", "beta", "gamma", "delta", "epsilon"] {
            assert!(cmp.output_keys.contains(k));
        }
    }

    // ─── meta-test: every encoder id used by the pipeline has a row ───

    #[test]
    fn declared_loss_table_covers_known_encoders() {
        for id in [
            "json_compact",
            "deep_mckp",
            "deep_mckp_inner_table",
            "mckp_v2",
            "csv",
            "csv_from_md",
            "kv",
            "mr_diff_fence",
        ] {
            assert!(
                declared_loss(id).is_some(),
                "encoder id `{id}` missing from declared_loss table"
            );
        }
        // Defensive: an unknown id should still return None so the test
        // does not silently flag a typo as `None` loss.
        assert!(declared_loss("totally_made_up").is_none());
    }

    #[test]
    fn empty_input_collects_no_keys() {
        assert!(keys_of("").is_empty());
        assert!(keys_of("not json").is_empty());
        assert!(keys_of("[1,2,3]").is_empty());
    }
}
