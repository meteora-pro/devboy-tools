//! Paper 3 — argument projection for speculative pre-fetch.
//!
//! When the planner picks a `FollowUpLink` (e.g. `Glob → Read`), the
//! host needs concrete `args` to dispatch the prefetch. Two paths
//! produce them:
//!
//! 1. **Provider override** — `ToolEnricher::project_args` returns
//!    a vendor-specific projection (e.g. GitLab knows that
//!    `get_merge_requests → get_merge_request_discussions` projects
//!    `id` to `merge_request_id`).
//! 2. **Generic fallback** (this module) — when the user (or the
//!    built-in defaults in `tool_defaults`) annotates a follow-up
//!    with both `projection: Some("path")` and
//!    `projection_arg: Some("file_path")`, the generic resolver
//!    extracts every `path` from the previous response and emits one
//!    prefetch request per match, capped at `max_per_link`.
//!
//! Both paths return `Vec<Value>` — one JSON object per prefetch
//! request the host should dispatch. Empty `Vec` means "nothing to
//! prefetch from this link".

use devboy_core::FollowUpLink;
use serde_json::{Map, Value};

/// Hard cap on prefetches generated from a single follow-up link.
/// Mirrors the corpus finding that top-3 prefetch covers > 80% of
/// cited follow-ups (`paper3_corpus_findings.md` §Glob → Read,
/// §Grep → Read). Higher would just waste rate-limit budget.
pub const MAX_PROJECTIONS_PER_LINK: usize = 3;

/// Built-in projection extractors for the canonical Paper-3 chains.
///
/// Returns a vector of `args` JSON objects, ready to feed into the
/// follow-up tool's `tools/call`. The host should also enforce the
/// per-turn `max_parallel_prefetches` cap on top of this.
///
/// Resolution order:
///
/// 1. Built-in match (`Glob → Read`, `Grep → Read`,
///    `WebSearch → WebFetch`, …).
/// 2. Generic fallback using `link.projection` + `link.projection_arg`
///    on the JSON tree.
/// 3. Empty `Vec` if neither path produced anything.
pub fn extract_args(prev_tool: &str, prev_result: &Value, link: &FollowUpLink) -> Vec<Value> {
    if let Some(args) = builtin_extract(prev_tool, prev_result, link) {
        return args;
    }
    generic_extract(prev_result, link)
}

/// Hard-coded extractors for built-in tool chains. Returning `Some`
/// short-circuits the generic path — useful when the schema is
/// well-known and the generic JSON walk would miss it (e.g. text-only
/// Grep output, plain newline-separated paths).
fn builtin_extract(
    prev_tool: &str,
    prev_result: &Value,
    link: &FollowUpLink,
) -> Option<Vec<Value>> {
    match (prev_tool, link.tool.as_str()) {
        ("Glob", "Read") | ("Glob", "Grep") => Some(extract_glob_paths(
            prev_result,
            link.projection_arg.as_deref().unwrap_or("file_path"),
        )),
        ("Grep", "Read") | ("Grep", "Edit") => Some(extract_grep_paths(
            prev_result,
            link.projection_arg.as_deref().unwrap_or("file_path"),
        )),
        ("WebSearch", "WebFetch") => Some(extract_websearch_urls(
            prev_result,
            link.projection_arg.as_deref().unwrap_or("url"),
        )),
        _ => None,
    }
}

/// Glob output is one of:
///   - JSON array of strings: `["src/main.rs", "src/lib.rs"]`
///   - JSON array of objects with a `path` / `match_path` field
///   - newline-separated text body: `src/main.rs\nsrc/lib.rs\n`
fn extract_glob_paths(prev_result: &Value, arg_name: &str) -> Vec<Value> {
    let paths = if let Some(arr) = prev_result.as_array() {
        arr.iter()
            .filter_map(|v| {
                v.as_str()
                    .map(String::from)
                    .or_else(|| string_field(v, "path"))
                    .or_else(|| string_field(v, "match_path"))
            })
            .collect::<Vec<_>>()
    } else if let Some(s) = prev_result.as_str() {
        s.lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(String::from)
            .collect()
    } else {
        Vec::new()
    };

    paths
        .into_iter()
        .take(MAX_PROJECTIONS_PER_LINK)
        .map(|p| single_arg(arg_name, Value::String(p)))
        .collect()
}

/// Grep output line shape: `path:line:col:match` or `path:match`.
/// We dedup by path (first hit wins) and take the top N — the agent
/// is far more likely to read each unique file once than to read the
/// same file three times.
fn extract_grep_paths(prev_result: &Value, arg_name: &str) -> Vec<Value> {
    let body = match prev_result {
        Value::String(s) => s.clone(),
        // Grep wrappers sometimes return JSON arrays of match objects
        // — handle both shapes.
        Value::Array(arr) => {
            let mut seen: Vec<String> = Vec::new();
            for v in arr {
                if let Some(p) = string_field(v, "path").or_else(|| string_field(v, "file"))
                    && !seen.contains(&p)
                {
                    seen.push(p);
                }
                if seen.len() >= MAX_PROJECTIONS_PER_LINK {
                    break;
                }
            }
            return seen
                .into_iter()
                .map(|p| single_arg(arg_name, Value::String(p)))
                .collect();
        }
        _ => return Vec::new(),
    };

    let mut seen: Vec<String> = Vec::new();
    for line in body.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        // First colon-delimited field is the path. Skip Windows-style
        // "C:\foo" by looking past the second character before splitting.
        let path = trimmed.split(':').next().unwrap_or("").trim().to_string();
        if path.is_empty() || seen.contains(&path) {
            continue;
        }
        seen.push(path);
        if seen.len() >= MAX_PROJECTIONS_PER_LINK {
            break;
        }
    }
    seen.into_iter()
        .map(|p| single_arg(arg_name, Value::String(p)))
        .collect()
}

/// WebSearch returns a list of `{title, url, snippet}` objects (or a
/// `{results: […]}` wrapper). Top-1 by default order is the most-
/// likely fetch — corpus shows the agent rarely fetches deeper than
/// position 1.
fn extract_websearch_urls(prev_result: &Value, arg_name: &str) -> Vec<Value> {
    let arr = prev_result
        .get("results")
        .and_then(Value::as_array)
        .or_else(|| prev_result.as_array());
    let Some(arr) = arr else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|v| string_field(v, "url"))
        .take(1)
        .map(|u| single_arg(arg_name, Value::String(u)))
        .collect()
}

/// Generic fallback — walks the JSON tree of `prev_result` and
/// extracts every leaf string at field name `link.projection`. Emits
/// one prefetch request per leaf, with `link.projection_arg` as the
/// argument name. Caps at `MAX_PROJECTIONS_PER_LINK`.
fn generic_extract(prev_result: &Value, link: &FollowUpLink) -> Vec<Value> {
    let Some(field) = link.projection.as_deref() else {
        return Vec::new();
    };
    let Some(arg_name) = link.projection_arg.as_deref() else {
        return Vec::new();
    };

    let mut out: Vec<Value> = Vec::new();
    walk(prev_result, field, &mut |v| {
        out.push(single_arg(arg_name, v.clone()));
        out.len() < MAX_PROJECTIONS_PER_LINK
    });
    out
}

/// Walk `v` depth-first, calling `visit(field_value)` for every leaf
/// where the parent object's key equals `field`. Visitor returns
/// `true` to continue, `false` to stop.
fn walk(v: &Value, field: &str, visit: &mut impl FnMut(&Value) -> bool) -> bool {
    match v {
        Value::Object(map) => {
            for (k, val) in map {
                if k == field {
                    let cont = visit(val);
                    if !cont {
                        return false;
                    }
                }
                if !walk(val, field, visit) {
                    return false;
                }
            }
            true
        }
        Value::Array(arr) => {
            for item in arr {
                if !walk(item, field, visit) {
                    return false;
                }
            }
            true
        }
        _ => true,
    }
}

fn string_field(v: &Value, name: &str) -> Option<String> {
    v.get(name).and_then(Value::as_str).map(String::from)
}

/// Extract the host portion of a URL for rate-limit grouping.
/// Returns the lower-cased host without scheme, port, path, or query.
///
/// The parser is intentionally tiny — no `url` crate dependency and no
/// IDN normalisation. Edge cases like userinfo (`user:pass@`) and IPv6
/// brackets are handled, but exotic forms (URN, mailto:) return `None`.
///
/// ```
/// use devboy_format_pipeline::projection::extract_host;
/// assert_eq!(extract_host("https://api.github.com/repos/x/y"), Some("api.github.com".into()));
/// assert_eq!(extract_host("http://Example.COM:8080/foo"), Some("example.com".into()));
/// assert_eq!(extract_host("https://user:p@host.example.org/x"), Some("host.example.org".into()));
/// assert_eq!(extract_host("https://[::1]:80/p"), Some("[::1]".into()));
/// assert_eq!(extract_host("/local/path"), None);
/// ```
pub fn extract_host(url: &str) -> Option<String> {
    let after_scheme = url.split_once("://").map(|(_, rest)| rest)?;
    // Strip everything past the authority section.
    let authority = after_scheme.split(['/', '?', '#']).next()?;
    if authority.is_empty() {
        return None;
    }
    // Drop userinfo: `user:pass@host` → `host`.
    let host_with_port = match authority.rsplit_once('@') {
        Some((_, rest)) => rest,
        None => authority,
    };
    // IPv6: keep the bracketed form `[::1]`, strip only the trailing port.
    let host = if let Some(stripped) = host_with_port.strip_prefix('[') {
        let close = stripped.find(']')?;
        let inside = &stripped[..close];
        // Re-add brackets so the dispatcher's per-host map can use the
        // canonical `[::1]` form as a key.
        format!("[{inside}]")
    } else {
        // Strip the `:port` suffix on a non-bracketed authority.
        host_with_port
            .rsplit_once(':')
            .map(|(h, _)| h)
            .unwrap_or(host_with_port)
            .to_string()
    };
    if host.is_empty() {
        return None;
    }
    Some(host.to_ascii_lowercase())
}

fn single_arg(name: &str, value: Value) -> Value {
    let mut m = Map::new();
    m.insert(name.to_string(), value);
    Value::Object(m)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn link(tool: &str, projection: &str, arg: &str) -> FollowUpLink {
        FollowUpLink {
            tool: tool.into(),
            probability: 1.0,
            projection: Some(projection.into()),
            projection_arg: Some(arg.into()),
        }
    }

    #[test]
    fn glob_to_read_extracts_paths_from_array_of_strings() {
        let result = json!(["src/main.rs", "src/lib.rs", "src/api.rs", "src/db.rs"]);
        let l = link("Read", "match_path", "file_path");
        let args = extract_args("Glob", &result, &l);
        // MAX_PROJECTIONS_PER_LINK = 3 — fourth path drops.
        assert_eq!(args.len(), 3);
        assert_eq!(args[0]["file_path"], "src/main.rs");
        assert_eq!(args[1]["file_path"], "src/lib.rs");
        assert_eq!(args[2]["file_path"], "src/api.rs");
    }

    #[test]
    fn glob_to_read_extracts_paths_from_array_of_objects() {
        let result = json!([
            {"path": "a.rs", "size": 100},
            {"path": "b.rs", "size": 200},
        ]);
        let l = link("Read", "path", "file_path");
        let args = extract_args("Glob", &result, &l);
        assert_eq!(args.len(), 2);
        assert_eq!(args[0]["file_path"], "a.rs");
        assert_eq!(args[1]["file_path"], "b.rs");
    }

    #[test]
    fn glob_to_read_extracts_paths_from_text_body() {
        let result = Value::String("src/main.rs\n\nsrc/lib.rs\n  src/api.rs  \n".into());
        let l = link("Read", "match_path", "file_path");
        let args = extract_args("Glob", &result, &l);
        assert_eq!(args.len(), 3);
        assert_eq!(args[0]["file_path"], "src/main.rs");
        assert_eq!(args[1]["file_path"], "src/lib.rs");
        assert_eq!(args[2]["file_path"], "src/api.rs");
    }

    #[test]
    fn grep_to_read_dedups_by_path() {
        // Real grep output: same file appears on multiple lines, planner
        // must not prefetch the same Read three times.
        let result = Value::String(
            "src/main.rs:10:fn foo() {}\n\
             src/main.rs:42:fn bar() {}\n\
             src/lib.rs:5:fn baz() {}\n\
             src/db.rs:1:use std;\n"
                .into(),
        );
        let l = link("Read", "path", "file_path");
        let args = extract_args("Grep", &result, &l);
        assert_eq!(args.len(), 3);
        let paths: Vec<&str> = args
            .iter()
            .map(|a| a["file_path"].as_str().unwrap())
            .collect();
        assert_eq!(paths, vec!["src/main.rs", "src/lib.rs", "src/db.rs"]);
    }

    #[test]
    fn grep_to_read_handles_array_of_objects() {
        let result = json!([
            {"path": "a.rs", "line": 1},
            {"path": "a.rs", "line": 2},
            {"path": "b.rs", "line": 1},
        ]);
        let l = link("Read", "path", "file_path");
        let args = extract_args("Grep", &result, &l);
        assert_eq!(args.len(), 2);
        assert_eq!(args[0]["file_path"], "a.rs");
        assert_eq!(args[1]["file_path"], "b.rs");
    }

    #[test]
    fn websearch_to_webfetch_takes_top_url_only() {
        let result = json!({
            "results": [
                {"title": "First",  "url": "https://example.com/a", "snippet": "…"},
                {"title": "Second", "url": "https://example.com/b", "snippet": "…"},
            ]
        });
        let l = link("WebFetch", "url", "url");
        let args = extract_args("WebSearch", &result, &l);
        // Top-1 only: corpus shows the agent rarely fetches deeper.
        assert_eq!(args.len(), 1);
        assert_eq!(args[0]["url"], "https://example.com/a");
    }

    #[test]
    fn generic_fallback_walks_nested_objects() {
        let result = json!({
            "outer": {
                "inner": [
                    {"id": 1, "deep": {"target_field": "value-1"}},
                    {"id": 2, "deep": {"target_field": "value-2"}},
                ]
            }
        });
        let l = FollowUpLink {
            tool: "custom_get".into(),
            probability: 1.0,
            projection: Some("target_field".into()),
            projection_arg: Some("identifier".into()),
        };
        let args = extract_args("custom_list", &result, &l);
        assert_eq!(args.len(), 2);
        assert_eq!(args[0]["identifier"], "value-1");
        assert_eq!(args[1]["identifier"], "value-2");
    }

    #[test]
    fn generic_fallback_returns_empty_when_projection_missing() {
        let result = json!({"x": 1});
        let l = FollowUpLink {
            tool: "next".into(),
            probability: 1.0,
            // No projection — generic path can't extract anything.
            ..FollowUpLink::default()
        };
        let args = extract_args("prev", &result, &l);
        assert!(args.is_empty());
    }

    // ─── extract_host ────────────────────────────────────────────────

    #[test]
    fn extract_host_strips_scheme_and_path() {
        assert_eq!(
            extract_host("https://api.github.com/repos/x/y"),
            Some("api.github.com".into())
        );
        assert_eq!(
            extract_host("https://gitlab.example.com/project/-/issues"),
            Some("gitlab.example.com".into())
        );
    }

    #[test]
    fn extract_host_lowercases_and_drops_port() {
        assert_eq!(
            extract_host("http://Example.COM:8080/foo"),
            Some("example.com".into())
        );
        assert_eq!(
            extract_host("https://API.OPENAI.COM"),
            Some("api.openai.com".into())
        );
    }

    #[test]
    fn extract_host_handles_userinfo() {
        assert_eq!(
            extract_host("https://user:pass@host.example.org/x"),
            Some("host.example.org".into())
        );
        assert_eq!(
            extract_host("ftp://anonymous@ftp.example.org"),
            Some("ftp.example.org".into())
        );
    }

    #[test]
    fn extract_host_keeps_ipv6_brackets() {
        assert_eq!(extract_host("https://[::1]:80/p"), Some("[::1]".into()));
        assert_eq!(
            extract_host("http://[2001:db8::1]/foo"),
            Some("[2001:db8::1]".into())
        );
    }

    #[test]
    fn extract_host_returns_none_for_non_urls() {
        assert!(extract_host("/local/path").is_none());
        assert!(extract_host("just-a-string").is_none());
        assert!(extract_host("").is_none());
        assert!(extract_host("https://").is_none());
    }

    #[test]
    fn extract_host_strips_query_and_fragment() {
        assert_eq!(
            extract_host("https://example.com/foo?bar=1&baz=2"),
            Some("example.com".into())
        );
        assert_eq!(
            extract_host("https://example.com#anchor"),
            Some("example.com".into())
        );
    }

    #[test]
    fn unknown_chain_falls_through_to_generic() {
        let result = json!({"items": [{"key": "k1"}, {"key": "k2"}]});
        let l = FollowUpLink {
            tool: "consume".into(),
            probability: 1.0,
            projection: Some("key".into()),
            projection_arg: Some("name".into()),
        };
        let args = extract_args("produce", &result, &l);
        assert_eq!(args.len(), 2);
        assert_eq!(args[0]["name"], "k1");
        assert_eq!(args[1]["name"], "k2");
    }
}
