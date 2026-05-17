//! CI grep gate per ADR-023 §3.7 third bullet of "agent never
//! sees the value": every `.expose_secret()` call in this
//! crate must be in the vetted allowlist below.
//!
//! What this test catches:
//!
//! - A new MCP tool handler that accidentally serialises a
//!   secret value into the JSON-RPC reply.
//! - A telemetry / logging path that pulls the value out of
//!   `SecretString` to log it.
//! - A refactor that moves a legitimate `.expose_secret()`
//!   call (proxy auth, telemetry submission) into a file the
//!   agent surface lives in, blurring the trust boundary.
//!
//! When this test fails, the right answer is almost always to
//! restructure the code so the value stays inside the
//! `SecretString` until the **leaf network call** that needs
//! it. Adding to the allowlist is a last resort and requires a
//! sentence explaining why the call is agent-safe.
//!
//! Run with: `cargo test -p devboy-mcp --test no_expose_secret_outside_allowlist`.

use std::fs;
use std::path::{Path, PathBuf};

/// Files in `crates/devboy-mcp/src/` allowed to call
/// `.expose_secret()`. Each entry needs a one-sentence reason —
/// future maintainers add to this list only after confirming the
/// site doesn't reach the agent.
const ALLOWED_FILES: &[(&str, &str)] = &[
    (
        "proxy.rs",
        "Forwards Bearer/Basic auth header to upstream MCP servers — \
         token never appears in any tool response.",
    ),
    (
        "proxy_secrets.rs",
        "Resolves secret aliases for outgoing HTTP requests — value \
         is consumed by the proxy layer only.",
    ),
    (
        "telemetry.rs",
        "Sentry DSN auth header — value goes to Sentry, never to the \
         agent.",
    ),
    (
        "agent_safety.rs",
        "Docs/comments mention the term as part of the gate's \
         documentation.",
    ),
];

#[test]
fn expose_secret_only_appears_in_allowlisted_mcp_files() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let src_root = crate_root.join("src");

    let mut violations: Vec<String> = Vec::new();
    walk_rust_files(&src_root, &mut |path: &Path| {
        let body = fs::read_to_string(path).unwrap_or_default();
        if !body.contains(".expose_secret(") {
            return;
        }
        let rel = path.strip_prefix(&src_root).unwrap();
        let rel_str = rel.to_string_lossy().replace('\\', "/");
        let allowed = ALLOWED_FILES.iter().any(|(name, _)| {
            // Match the leaf filename — the allowlist intentionally
            // doesn't pin the directory so a refactor that moves a
            // file (without renaming) still passes.
            rel_str.ends_with(name)
        });
        if !allowed {
            for (line_no, line) in body.lines().enumerate() {
                if line.contains(".expose_secret(") {
                    violations.push(format!("{}:{}: {}", rel_str, line_no + 1, line.trim()));
                }
            }
        }
    });

    if !violations.is_empty() {
        let allowlist = ALLOWED_FILES
            .iter()
            .map(|(name, why)| format!("  - {name}: {why}"))
            .collect::<Vec<_>>()
            .join("\n");
        panic!(
            "ADR-023 §3.7 violation: `.expose_secret(` found in non-allowlisted \
             devboy-mcp files. Either restructure the code to keep the value \
             inside SecretString, or add the file to ALLOWED_FILES with a \
             one-sentence reason it's agent-safe.\n\n\
             Violations:\n{}\n\nCurrent allowlist:\n{}",
            violations.join("\n"),
            allowlist
        );
    }
}

#[test]
fn allowlist_entries_actually_match_real_files() {
    // Defensive: an allowlist entry pointing at a deleted file
    // would silently weaken the gate. Catch it here.
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let src_root = crate_root.join("src");
    let mut found: Vec<String> = Vec::new();
    walk_rust_files(&src_root, &mut |path: &Path| {
        if let Some(name) = path.file_name().and_then(|s| s.to_str()) {
            found.push(name.to_owned());
        }
    });
    for (name, _) in ALLOWED_FILES {
        assert!(
            found.iter().any(|f| f == name),
            "ALLOWED_FILES entry `{name}` does not match any file under \
             crates/devboy-mcp/src/. Either remove the entry or restore the \
             file."
        );
    }
}

fn walk_rust_files(dir: &Path, sink: &mut dyn FnMut(&Path)) {
    let Ok(read) = fs::read_dir(dir) else {
        return;
    };
    for entry in read.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk_rust_files(&path, sink);
        } else if path.extension().and_then(|s| s.to_str()) == Some("rs") {
            sink(&path);
        }
    }
}
