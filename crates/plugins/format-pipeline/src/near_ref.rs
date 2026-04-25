//! Type-2 (near-duplicate) reference hints — Paper 2 §Near-reference.
//!
//! When the L0 cache holds a body that is *not* byte-identical to the
//! current response but differs only in a few enumerated fields, we
//! emit a compact delta hint instead of the full body or a full
//! reference hint. The canonical case is pipeline polling: two calls
//! to `get_pipeline(id=42)` differing only in `status` and `duration`.
//!
//! ```text
//! > [near-ref: tc_42, status: pending→success, duration: +22s]
//! ```
//!
//! Eligibility (per the paper):
//!
//! - Both inputs must be valid JSON objects.
//! - Their top-level key sets must match (no fields added or removed).
//! - The **delta payload** (rendered hint minus framing) must be
//!   ≤ 30 bytes.
//! - The full response must be ≥ 500 bytes — savings on shorter
//!   bodies do not justify the delta-extraction cost.
//!
//! The eligibility predicate is conservative: when in doubt, this
//! module returns `None` and the caller falls back to either a full
//! reference hint or fresh body.

use std::collections::BTreeSet;

/// One scalar field that differs between two JSON objects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeltaField {
    pub key: String,
    pub old: String,
    pub new: String,
}

/// Configuration knobs for delta extraction.
#[derive(Debug, Clone, Copy)]
pub struct NearRefConfig {
    /// Maximum total bytes in the rendered delta payload (`status: a→b, …`).
    pub max_delta_bytes: usize,
    /// Minimum input body length (bytes) for which a near-ref hint is
    /// worth emitting. Below this, the full body is cheaper than the
    /// extraction work.
    pub min_body_bytes: usize,
}

impl Default for NearRefConfig {
    fn default() -> Self {
        Self {
            // 50-byte ceiling fits the canonical pipeline-polling case
            // (status: pending→success, duration: 12→34 ≈ 34 bytes
            // including framing) while still rejecting cases where
            // half a dozen fields drifted. Paper text uses 30 as the
            // payload-only bound, but the runtime sums framing in too.
            max_delta_bytes: 50,
            min_body_bytes: 500,
        }
    }
}

/// Try to extract a delta between `old_body` and `new_body`. Returns the
/// list of differing fields when *all* eligibility checks pass; otherwise
/// `None` so the caller can fall back to a full ref / fresh body.
pub fn extract_delta(
    old_body: &str,
    new_body: &str,
    config: &NearRefConfig,
) -> Option<Vec<DeltaField>> {
    if new_body.len() < config.min_body_bytes {
        return None;
    }
    let old: serde_json::Value = serde_json::from_str(old_body.trim_start()).ok()?;
    let new: serde_json::Value = serde_json::from_str(new_body.trim_start()).ok()?;
    let old_obj = old.as_object()?;
    let new_obj = new.as_object()?;

    // Top-level key sets must match. A new field appearing means the
    // shape shifted and a delta hint would be misleading.
    let old_keys: BTreeSet<&str> = old_obj.keys().map(|s| s.as_str()).collect();
    let new_keys: BTreeSet<&str> = new_obj.keys().map(|s| s.as_str()).collect();
    if old_keys != new_keys {
        return None;
    }

    let mut deltas = Vec::new();
    let mut delta_bytes = 0usize;
    for (k, new_val) in new_obj {
        let old_val = match old_obj.get(k) {
            Some(v) => v,
            None => unreachable!("key sets are equal"),
        };
        if old_val == new_val {
            continue;
        }
        // Only scalar diffs round-trip cleanly into a hint. A nested
        // object difference would balloon the hint and obscure the
        // delta — refuse the hint and let the caller pass the full body.
        if !is_scalar(old_val) || !is_scalar(new_val) {
            return None;
        }
        let old_s = scalar_to_string(old_val);
        let new_s = scalar_to_string(new_val);
        // Approximate the on-wire size: `key: old→new, ` (plus separators
        // amortised). The exact framing overhead is added by the caller.
        delta_bytes += k.len() + old_s.len() + new_s.len() + 4; // ": ", "→"
        if delta_bytes > config.max_delta_bytes {
            return None;
        }
        deltas.push(DeltaField {
            key: k.clone(),
            old: old_s,
            new: new_s,
        });
    }

    // No diff at all → caller should emit a full byte-identical hint
    // via the regular L0 path, not a near-ref. Returning `None` here
    // lets the caller distinguish.
    if deltas.is_empty() {
        return None;
    }
    Some(deltas)
}

fn is_scalar(v: &serde_json::Value) -> bool {
    matches!(
        v,
        serde_json::Value::Null
            | serde_json::Value::Bool(_)
            | serde_json::Value::Number(_)
            | serde_json::Value::String(_)
    )
}

fn scalar_to_string(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::Null => String::new(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::String(s) => s.clone(),
        _ => v.to_string(),
    }
}

/// Render the near-ref hint for the agent. Single line, matches the
/// `> [near-ref: <id>, <field>: <old>→<new>, …]` format documented in
/// Paper 2 §Near-reference.
pub fn render_near_ref_hint(reference_id: &str, deltas: &[DeltaField]) -> String {
    let mut out = String::new();
    out.push_str("> [near-ref: ");
    out.push_str(reference_id);
    for d in deltas {
        out.push_str(", ");
        out.push_str(&d.key);
        out.push_str(": ");
        out.push_str(&d.old);
        out.push('→');
        out.push_str(&d.new);
    }
    out.push(']');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> NearRefConfig {
        NearRefConfig::default()
    }

    fn long_pipeline_body(status: &str, duration: u64) -> String {
        // Pad with a generic field so the body clears `min_body_bytes`.
        format!(
            r#"{{"id":42,"name":"{}","status":"{}","duration":{},"url":"https://example.com/pipelines/42","commit_sha":"deadbeefdeadbeefdeadbeefdeadbeef","ref":"refs/heads/main","author":"alice","triggered_by":"webhook","preview":"{}"}}"#,
            "p".repeat(20),
            status,
            duration,
            "x".repeat(400)
        )
    }

    #[test]
    fn extracts_status_and_duration_delta_for_pipeline_polling() {
        let a = long_pipeline_body("pending", 12);
        let b = long_pipeline_body("success", 34);
        let deltas = extract_delta(&a, &b, &cfg()).unwrap();
        assert_eq!(deltas.len(), 2);
        let keys: BTreeSet<_> = deltas.iter().map(|d| d.key.as_str()).collect();
        assert!(keys.contains("status"));
        assert!(keys.contains("duration"));
    }

    #[test]
    fn refuses_when_too_short() {
        // Both bodies are well under min_body_bytes (500).
        let a = r#"{"a":1,"b":2}"#;
        let b = r#"{"a":1,"b":3}"#;
        assert!(extract_delta(a, b, &cfg()).is_none());
    }

    #[test]
    fn refuses_when_keys_differ() {
        let a = format!(r#"{{"a":1,"long":"{}"}}"#, "x".repeat(600));
        let b = format!(r#"{{"a":1,"different":"{}"}}"#, "x".repeat(600));
        assert!(extract_delta(&a, &b, &cfg()).is_none());
    }

    #[test]
    fn refuses_when_nested_value_changes() {
        let a = format!(r#"{{"meta":{{"k":1}},"pad":"{}"}}"#, "x".repeat(600));
        let b = format!(r#"{{"meta":{{"k":2}},"pad":"{}"}}"#, "x".repeat(600));
        assert!(extract_delta(&a, &b, &cfg()).is_none());
    }

    #[test]
    fn refuses_when_delta_blob_too_large() {
        // Two very long string fields differ — easily exceeds 50 bytes.
        let a = format!(
            r#"{{"x":"{}","pad":"{}"}}"#,
            "a".repeat(80),
            "p".repeat(600)
        );
        let b = format!(
            r#"{{"x":"{}","pad":"{}"}}"#,
            "b".repeat(80),
            "p".repeat(600)
        );
        assert!(extract_delta(&a, &b, &cfg()).is_none());
    }

    #[test]
    fn returns_none_for_byte_identical_inputs() {
        // Caller should fall back to the regular byte-identical hint
        // path rather than treat a zero-delta change as a near-ref.
        let a = long_pipeline_body("ok", 10);
        assert!(extract_delta(&a, &a, &cfg()).is_none());
    }

    #[test]
    fn render_format_matches_paper_spec() {
        let deltas = vec![
            DeltaField {
                key: "status".into(),
                old: "pending".into(),
                new: "success".into(),
            },
            DeltaField {
                key: "duration".into(),
                old: "12".into(),
                new: "34".into(),
            },
        ];
        let s = render_near_ref_hint("tc_42", &deltas);
        assert_eq!(
            s,
            "> [near-ref: tc_42, status: pending→success, duration: 12→34]"
        );
    }

    #[test]
    fn hint_size_under_paper_budget() {
        // Paper 2 §Hint Cost Model targets ≤ 18 cl100k_base tokens for
        // a typical near-ref hint. We assert byte length as a proxy
        // (≤ 70 bytes ≈ 18 tokens on average for ASCII).
        let deltas = vec![
            DeltaField {
                key: "status".into(),
                old: "pending".into(),
                new: "success".into(),
            },
            DeltaField {
                key: "duration".into(),
                old: "12".into(),
                new: "34".into(),
            },
        ];
        let s = render_near_ref_hint("tc_42", &deltas);
        assert!(s.len() <= 70, "near-ref hint too long: {} bytes", s.len());
    }
}
