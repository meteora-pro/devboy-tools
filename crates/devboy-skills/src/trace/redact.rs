//! Redaction of sensitive values before traces hit disk.
//!
//! Two mechanisms are layered:
//!
//! 1. Known credential shapes are masked regardless of where they
//!    appear in the tree. Currently: `ghp_`, `glpat-`, `pk_`, `sk-`,
//!    `xoxb-` / `xoxa-` / `xapp-`, `Bearer ` / `Basic ` (case-
//!    insensitive), plus a few other common prefixes. These all
//!    survive without knowing the configured credential set — useful
//!    when a token leaks into an error message, a git URL, or a
//!    user-supplied prompt.
//! 2. Values of any string-valued environment variable whose name
//!    matches a sensitive suffix (`*_TOKEN` / `*_SECRET` / `*_KEY` /
//!    `*_PASSWORD` / `*_PASSPHRASE` / `AUTHORIZATION` / `COOKIE`) are
//!    masked — the redactor snapshots those at call time.
//!
//! Setting the `DEVBOY_TRACE_REDACTION=off` environment variable
//! disables both passes for local debugging. Never default to off.
//!
//! ## Amortizing the env snapshot
//!
//! The top-level [`sanitize`] helper walks `std::env::vars()` on every
//! call — fine for one-shot CLI invocations but wasteful inside a
//! long-running producer like [`super::SessionTracer`] that writes
//! many events. Build a [`Redactor`] once with
//! [`Redactor::snapshot`] and reuse it for every event in the same
//! session to pay the env scan just once.

use std::collections::HashSet;

use serde_json::Value;

/// Redact sensitive data in `value`. Recursively walks maps and
/// arrays. Strings are rewritten; numbers / bools / null pass through
/// unchanged.
///
/// Each call snapshots `*_TOKEN` / `*_SECRET` / … env vars afresh so
/// that tests using `temp_env::with_var` (and production callers that
/// legitimately mutate the environment) see up-to-date state. Inside
/// a long session, prefer [`Redactor::snapshot`] + [`Redactor::sanitize`].
pub fn sanitize(value: Value) -> Value {
    Redactor::snapshot().sanitize(value)
}

/// A reusable redactor that holds one env-var snapshot. Created via
/// [`Redactor::snapshot`]; use once per long-running producer (e.g.
/// one per `SessionTracer`) to avoid rescanning the environment on
/// every event.
#[derive(Debug, Clone)]
pub struct Redactor {
    enabled: bool,
    secrets: HashSet<String>,
}

impl Redactor {
    /// Capture the current set of sensitive env-var values and the
    /// `DEVBOY_TRACE_REDACTION=off` opt-out state. Cheap to clone.
    pub fn snapshot() -> Self {
        if redaction_disabled() {
            Self {
                enabled: false,
                secrets: HashSet::new(),
            }
        } else {
            Self {
                enabled: true,
                secrets: known_env_secrets(),
            }
        }
    }

    /// Sanitize a single value using the captured env-var snapshot.
    pub fn sanitize(&self, value: Value) -> Value {
        if !self.enabled {
            return value;
        }
        sanitize_with(&self.secrets, value)
    }
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
    // Case-sensitive prefixes. The publisher-defined provider tokens
    // are all case-sensitive in the wild, so matching them strictly
    // avoids redacting words that merely share the letters (e.g. an
    // English sentence starting with "Ghp").
    const CASE_SENSITIVE: &[&str] = &[
        // GitHub PATs
        "ghp_",
        "github_pat_",
        "gho_",
        "ghu_",
        "ghs_",
        "ghr_",
        // GitLab PATs
        "glpat-",
        // Publishable / secret key families shared across a few
        // providers (Stripe, ClickUp, etc.). ADR-015 spec calls these
        // out as a single `pk_` / `sk_` group — keep them generic.
        "pk_",
        "sk_",
        // OpenAI-ish (also covers sk-ant-… via the `sk-` prefix).
        "sk-",
        // Slack
        "xoxb-",
        "xoxa-",
        "xoxp-",
        "xapp-",
    ];
    if CASE_SENSITIVE
        .iter()
        .any(|p| s.starts_with(p) && s.len() > p.len() + 8)
    {
        return true;
    }
    // Case-insensitive auth-scheme prefixes: HTTP scheme tokens are
    // case-insensitive per RFC 7235, so `Bearer <tok>`, `bearer <tok>`
    // and `BEARER <tok>` should all redact.
    const SCHEME_CI: &[&str] = &["bearer ", "basic "];
    let lower = s.to_ascii_lowercase();
    SCHEME_CI
        .iter()
        .any(|p| lower.starts_with(p) && s.len() > p.len() + 8)
}

fn mask_auth_header_segment(s: &str) -> Option<String> {
    // e.g. "Authorization: Bearer ghp_…" embedded inside a log line.
    // HTTP auth schemes are case-insensitive (RFC 7235), so locate the
    // needle in the lowercased copy but preserve the original casing
    // of the scheme token in the rewritten output.
    let lower = s.to_ascii_lowercase();
    let needles = ["bearer ", "basic "];
    for needle in needles {
        if let Some(idx) = lower.find(needle) {
            let head = &s[..idx];
            let scheme = &s[idx..idx + needle.len()]; // original case preserved
            // Credential runs until whitespace, comma, or semicolon.
            let rest = &s[idx + needle.len()..];
            let end = rest
                .find(|c: char| c.is_whitespace() || c == ',' || c == ';')
                .unwrap_or(rest.len());
            if end >= 8 {
                let tail = &rest[end..];
                return Some(format!("{head}{scheme}<redacted:auth>{tail}"));
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
    use std::sync::Mutex;

    /// Serialise every test in this module around the process-wide
    /// environment. Two tests legitimately toggle
    /// `DEVBOY_TRACE_REDACTION=off` via `temp_env::with_var`, and
    /// `cargo test` runs the others concurrently — without this mutex
    /// a sibling test's `off` setting can leak into an unrelated test
    /// for the window it holds the var, making
    /// `masks_bare_bearer_value_case_insensitive` and friends flake
    /// on CI. The mutex is cheap (only contended during tests) and
    /// keeps the production code path zero-overhead. Combined with
    /// `temp_env::with_var`'s own save/restore logic this gives the
    /// whole module deterministic env state.
    static ENV_TEST_MUTEX: Mutex<()> = Mutex::new(());

    /// Helper: acquire the module-wide env-serialisation lock and run
    /// `f` inside a `temp_env` guard that explicitly UNsets
    /// `DEVBOY_TRACE_REDACTION`. Used by every test that expects the
    /// default (enabled) redactor. Without this wrapper a sibling
    /// test's `DEVBOY_TRACE_REDACTION=off` setting could race in and
    /// silently disable redaction mid-assertion.
    fn with_clean_env<R>(f: impl FnOnce() -> R) -> R {
        let _guard = ENV_TEST_MUTEX.lock().unwrap_or_else(|p| p.into_inner());
        temp_env::with_var("DEVBOY_TRACE_REDACTION", None::<&str>, f)
    }

    #[test]
    fn masks_github_pat() {
        with_clean_env(|| {
            let v = json!({ "args": { "token": "ghp_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" } });
            let out = sanitize(v);
            let s = serde_json::to_string(&out).unwrap();
            assert!(!s.contains("ghp_aaaaaaaa"));
            assert!(s.contains("<redacted"));
        });
    }

    #[test]
    fn masks_bearer_scheme_in_header_string() {
        with_clean_env(|| {
            let v = json!("Authorization: Bearer xxxxxxxxxxxxyyyyyyyyyyyy");
            let out = sanitize(v);
            let s = out.as_str().unwrap();
            assert!(!s.contains("xxxxxxxxxxxxyyyyyyyyyyyy"), "got: {s}");
            assert!(s.contains("<redacted"), "got: {s}");
        });
    }

    #[test]
    fn masks_by_key_name_even_when_value_looks_harmless() {
        with_clean_env(|| {
            // A value that does not match any known prefix but lives under
            // a key called `password` must still be redacted.
            let v = json!({ "password": "not-a-prefix" });
            let out = sanitize(v);
            assert_eq!(
                out.get("password").and_then(|v| v.as_str()),
                Some("<redacted:secret-field>")
            );
        });
    }

    #[test]
    fn env_var_values_are_redacted_when_they_match_exactly() {
        let _guard = ENV_TEST_MUTEX.lock().unwrap_or_else(|p| p.into_inner());
        temp_env::with_vars(
            [
                ("DEVBOY_TRACE_REDACTION", None::<&str>),
                (
                    "DEVBOY_TEST_TOKEN",
                    Some("super-secret-value-nothing-matches"),
                ),
            ],
            || {
                let v = json!({ "note": "leaked: super-secret-value-nothing-matches" });
                let out = sanitize(v);
                // The exact-match secret replacement only fires when
                // the value IS the secret — not when it's embedded in
                // a larger string. Embedded leakage is the DLP case we
                // don't attempt to solve (see the doc comment).
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
        with_clean_env(|| {
            // `ghp_` alone must not be redacted — only long PAT-shaped
            // strings are. This matters for documentation and for the
            // redaction marker itself.
            let v = json!("ghp_");
            assert_eq!(sanitize(v).as_str(), Some("ghp_"));
        });
    }

    #[test]
    fn redaction_can_be_disabled_via_env() {
        let _guard = ENV_TEST_MUTEX.lock().unwrap_or_else(|p| p.into_inner());
        temp_env::with_var("DEVBOY_TRACE_REDACTION", Some("off"), || {
            let v = json!({ "token": "ghp_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" });
            let out = sanitize(v.clone());
            assert_eq!(out, v);
        });
    }

    #[test]
    fn masks_bearer_scheme_case_insensitive() {
        with_clean_env(|| {
            // HTTP schemes are case-insensitive per RFC 7235, so all of
            // these variants must redact.
            for header in [
                "Authorization: Bearer xxxxxxxxxxxxyyyyyyyyyyyy",
                "authorization: bearer xxxxxxxxxxxxyyyyyyyyyyyy",
                "AUTHORIZATION: BEARER xxxxxxxxxxxxyyyyyyyyyyyy",
                "authorization: BeArEr xxxxxxxxxxxxyyyyyyyyyyyy",
            ] {
                let out = sanitize(json!(header));
                let s = out.as_str().unwrap();
                assert!(
                    !s.contains("xxxxxxxxxxxxyyyyyyyyyyyy"),
                    "token leaked for header `{header}` → `{s}`"
                );
                assert!(
                    s.contains("<redacted"),
                    "no redaction marker for header `{header}` → `{s}`"
                );
            }
        });
    }

    #[test]
    fn masks_bare_bearer_value_case_insensitive() {
        with_clean_env(|| {
            // When the caller pasted just the `Bearer <token>` segment as
            // a standalone value, the prefix check (not the header scanner)
            // fires — must also be case-insensitive.
            for raw in [
                "Bearer abcdefghijklmnopqrstuvwx",
                "bearer abcdefghijklmnopqrstuvwx",
                "BEARER abcdefghijklmnopqrstuvwx",
                "Basic YWxpY2U6aHVudGVyMjpkcmFnb24=",
            ] {
                let out = sanitize(json!(raw));
                let s = out.as_str().unwrap();
                assert!(s.contains("<redacted"), "not redacted: `{raw}` → `{s}`");
            }
        });
    }

    #[test]
    fn masks_generic_pk_prefix() {
        with_clean_env(|| {
            // ADR-015 calls out a generic `pk_` prefix (not just
            // `pk_live_` / `pk_test_`). Enough bytes after the prefix to
            // clear the length guard so a bare `pk_` literal is left alone.
            let v = json!({ "clickup_pk": "pk_abcdefghijklmnop" });
            let out = sanitize(v);
            assert_eq!(
                out.get("clickup_pk").and_then(|v| v.as_str()),
                Some("<redacted:token-pattern>"),
                "generic pk_ prefix should redact"
            );

            // Short `pk_` literal stays untouched (e.g. in docs).
            let doc = json!("pk_");
            assert_eq!(sanitize(doc).as_str(), Some("pk_"));
        });
    }

    #[test]
    fn redactor_snapshot_amortizes_env_scan() {
        let _guard = ENV_TEST_MUTEX.lock().unwrap_or_else(|p| p.into_inner());
        let redactor = temp_env::with_vars(
            [
                ("DEVBOY_TRACE_REDACTION", None::<&str>),
                ("DEVBOY_REDACTOR_CACHE_TOKEN", Some("cached-token-zzzzzzzz")),
            ],
            Redactor::snapshot,
        );
        // The env var is gone at this point, but the snapshot remembers.
        let out = redactor.sanitize(json!({ "raw": "cached-token-zzzzzzzz" }));
        assert_eq!(
            out.get("raw").and_then(|v| v.as_str()),
            Some("<redacted:credential>")
        );
    }

    #[test]
    fn redactor_snapshot_respects_disable_env() {
        let _guard = ENV_TEST_MUTEX.lock().unwrap_or_else(|p| p.into_inner());
        temp_env::with_var("DEVBOY_TRACE_REDACTION", Some("off"), || {
            let redactor = Redactor::snapshot();
            let v = json!({ "token": "ghp_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" });
            assert_eq!(redactor.sanitize(v.clone()), v);
        });
    }
}
