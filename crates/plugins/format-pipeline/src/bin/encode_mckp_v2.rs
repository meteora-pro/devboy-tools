//! Standalone CLI: encode JSON-Lines input with `deep_mckp_with_inner_table`.
//!
//! Protocol:
//!   stdin  — one JSON object per line: `{"record_id": "...", "raw": "<json string>"}`
//!   stdout — one JSON object per line: `{"record_id": "...", "encoded": "<encoded string>"}`
//!
//! Used by the Paper 2 reproducibility harness to validate the Rust port
//! against the Python reference encoder on real LLM comprehension runs.

use std::io::{self, BufRead, BufWriter, Write};

use devboy_format_pipeline::shape::classify;
use devboy_format_pipeline::templates::deep_mckp_with_inner_table;
use serde_json::Value;

fn encode_one(raw: &str) -> String {
    let cls = classify(raw);
    if let Some(out) = deep_mckp_with_inner_table(raw, &cls) {
        return out;
    }
    // Fallback to compact JSON (matches Python encoder's behaviour when no
    // inner homogeneous array is present).
    serde_json::from_str::<Value>(raw.trim_start())
        .ok()
        .and_then(|v| serde_json::to_string(&v).ok())
        .unwrap_or_else(|| raw.to_string())
}

fn main() -> io::Result<()> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut out = BufWriter::new(stdout.lock());

    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let req: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("parse error: {e}");
                continue;
            }
        };
        let record_id = req
            .get("record_id")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let raw = req
            .get("raw")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let encoded = encode_one(raw);
        let resp = serde_json::json!({
            "record_id": record_id,
            "encoded": encoded,
        });
        writeln!(out, "{}", serde_json::to_string(&resp)?)?;
    }
    out.flush()?;
    Ok(())
}
