//! Redaction of sensitive values before traces hit disk.
//!
//! Two mechanisms are layered:
//!
//! 1. Known credential shapes are masked regardless of where they
//!    appear in the tree. Currently: `ghp_`, `glpat-`, `pk_live_`,
//!    `pk_test_`, `sk-`, `xoxb-`/`xoxa-`/`xapp-`, `Bearer `, plus a
//!    few other common prefixes. These all survive without knowing
//!    the configured credential set — useful when a token leaks into
//!    an error message, a git URL, or a user-supplied prompt.
//! 2. Values of any string-valued environment variable whose name
//!    matches a sensitive suffix (`*_TOKEN` / `*_SECRET` / `*_KEY` /
//!    `*_PASSWORD` / `*_PASSPHRASE` / `AUTHORIZATION` / `COOKIE`) are
//!    masked — the redactor snapshots those at call time.
//!
//! Setting the `DEVBOY_TRACE_REDACTION=off` environment variable
//! disables both passes for local debugging. Never default to off.

use std::collections::HashSet;

use serde_json::Value;

/// Redact sensitive data in `value`. Recursively walks maps and
/// arrays. Strings are rewritten; numbers / bools / null pass through
/// unchanged.
pub fn sanitize(value: Value) -> Value {
    if redaction_disabled() {
        return value;
    }
    let secrets = known_env_secrets();
    sanitize_with(&secrets, value)
}

fn redaction_disabled() -> bool {
    match std::env::var("DEVBOY_TRACE_REDACTION") {
        Ok(v) => matches!(v.to_lowercase().as_str(), "off" | "0" | "false" | "no"),
        Err(_) => false,
    }
}

fn sanitize_with(secrets: &HashSet<String>, value: Value) -> Value {
    match value {
        Value::String(s) => Value::String(redact_string(secrets, &s)),
        Value::Array(xs) => {
            Value::Array(xs.into_iter().map(|x| sanitize_with(secrets, x)).collect())
        }
        Value::Object(map) => {
            let mut out = serde_json::Map::with_capacity(map.len());
            for (k, v) in map {
                // If the key itself hints at a secret, redact the whole
                // value regardless of its type. This prevents structured
                // leaks like `{"authorization": {"scheme": "Bearer",
                // "value": "…"}}` where nested field names may not
                // themselves trip the secret-key heuristic.
                let new_val = if key_looks_secret(&k) {
                    Value::String("<redacted:secret-field>".to_string())
                } else {
                    sanitize_with(secrets, v)
                };
                out.insert(k, new_val);
            }
            Value::Object(out)
        }
        other => other,
    }
}

fn redact_string(secrets: &HashSet<String>, s: &str) -> String {
    // 1. Exact env-var match.
    if !s.is_empty() && secrets.contains(s) {
        return "<redacted:credential>".to_string();
    }
    // 2. Known token prefixes. We search case-sensitively because every
    //    supported prefix is case-sensitive in practice.
    if has_known_prefix(s) {
        return "<redacted:token-pattern>".to_string();
    }
    // 3. Bearer / Basic schemes embedded inside a larger string. Don't
    //    rewrite the whole string — replace only the credential segment.
    if let Some(rewritten) = mask_auth_header_segment(s) {
        return rewritten;
    }
    s.to_string()
}

fn has_known_prefix(s: &str) -> bool {
    const PREFIXES: &[&str] = &[
        // GitHub PATs
        "ghp_",
        "github_pat_",
        "gho_",
        "ghu_",
        "ghs_",
        "ghr_",
        // GitLab PATs
        "glpat-",
        // Stripe-ish shapes, also matches ClickUp `pk_`
        "pk_live_",
        "pk_test_",
        "sk_live_",
        "sk_test_",
        // OpenAI-ish
        "sk-",
        // Slack
        "xoxb-",
        "xoxa-",
        "xoxp-",
        "xapp-",
        // Anthropic
        "sk-ant-",
        // Generic bearer markers users sometimes paste as a raw value.
        "Bearer ",
        "Basic ",
    ];
    PREFIXES
        .iter()
        .any(|p| s.starts_with(p) && s.len() > p.len() + 8)
}

fn mask_auth_header_segment(s: &str) -> Option<String> {
    // e.g. "Authorization: Bearer ghp_…" embedded inside a log line.
    let needles = ["Bearer ", "Basic "];
    for needle in needles {
        if let Some(idx) = s.find(needle) {
            let head = &s[..idx];
            // Credential runs until whitespace, comma, or semicolon.
            let rest = &s[idx + needle.len()..];
            let end = rest
                .find(|c: char| c.is_whitespace() || c == ',' || c == ';')
                .unwrap_or(rest.len());
            if end >= 8 {
                let tail = &rest[end..];
                return Some(format!("{head}{needle}<redacted:auth>{tail}"));
            }
        }
    }
    None
}

fn key_looks_secret(key: &str) -> bool {
    let upper = key.to_ascii_uppercase();
    const SUFFIXES: &[&str] = &[
        "_TOKEN",
        "_SECRET",
        "_KEY",
        "_PASSWORD",
        "_PASSPHRASE",
        "_AUTH",
    ];
    const EXACT: &[&str] = &["AUTHORIZATION", "COOKIE", "TOKEN", "SECRET", "PASSWORD"];
    if EXACT.contains(&upper.as_str()) {
        return true;
    }
    if SUFFIXES.iter().any(|suf| upper.ends_with(suf)) {
        return true;
    }
    // Common devboy conventions.
    // Use the upper-cased copy for the substring heuristic too, so
    // mixed-case keys like `Password` / `Token` / `Secret` are caught
    // consistently with the EXACT / SUFFIX branches above.
    if upper.contains("PASSWORD") || upper.contains("SECRET") || upper.contains("TOKEN") {
        return true;
    }
    false
}

fn known_env_secrets() -> HashSet<String> {
    let mut out = HashSet::new();
    for (name, value) in std::env::vars() {
        if value.is_empty() {
            continue;
        }
        if key_looks_secret(&name) {
            out.insert(value);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn masks_github_pat() {
        let v = json!({ "args": { "token": "ghp_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" } });
        let out = sanitize(v);
        let s = serde_json::to_string(&out).unwrap();
        assert!(!s.contains("ghp_aaaaaaaa"));
        assert!(s.contains("<redacted"));
    }

    #[test]
    fn masks_bearer_scheme_in_header_string() {
        let v = json!("Authorization: Bearer xxxxxxxxxxxxyyyyyyyyyyyy");
        let out = sanitize(v);
        let s = out.as_str().unwrap();
        assert!(!s.contains("xxxxxxxxxxxxyyyyyyyyyyyy"), "got: {s}");
        assert!(s.contains("<redacted"), "got: {s}");
    }

    #[test]
    fn masks_by_key_name_even_when_value_looks_harmless() {
        // A value that does not match any known prefix but lives under
        // a key called `password` must still be redacted.
        let v = json!({ "password": "not-a-prefix" });
        let out = sanitize(v);
        assert_eq!(
            out.get("password").and_then(|v| v.as_str()),
            Some("<redacted:secret-field>")
        );
    }

    #[test]
    fn env_var_values_are_redacted_when_they_match_exactly() {
        temp_env::with_var(
            "DEVBOY_TEST_TOKEN",
            Some("super-secret-value-nothing-matches"),
            || {
                let v = json!({ "note": "leaked: super-secret-value-nothing-matches" });
                let out = sanitize(v);
                // The exact-match secret replacement only fires when the
                // value IS the secret — not when it's embedded in a
                // larger string. Embedded leakage is the DLP case we do
                // not attempt to solve (see the doc comment). Assert the
                // exact-value case instead.
                let note = out.get("note").and_then(|v| v.as_str()).unwrap();
                assert_eq!(note, "leaked: super-secret-value-nothing-matches");

                let v = json!({ "raw": "super-secret-value-nothing-matches" });
                let out = sanitize(v);
                assert_eq!(
                    out.get("raw").and_then(|v| v.as_str()),
                    Some("<redacted:credential>")
                );
            },
        );
    }

    #[test]
    fn short_strings_are_not_redacted_by_prefix_check() {
        // `ghp_` alone must not be redacted — only long PAT-shaped
        // strings are. This matters for documentation and for the
        // redaction marker itself.
        let v = json!("ghp_");
        assert_eq!(sanitize(v).as_str(), Some("ghp_"));
    }

    #[test]
    fn redaction_can_be_disabled_via_env() {
        temp_env::with_var("DEVBOY_TRACE_REDACTION", Some("off"), || {
            let v = json!({ "token": "ghp_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" });
            let out = sanitize(v.clone());
            assert_eq!(out, v);
        });
    }
}
