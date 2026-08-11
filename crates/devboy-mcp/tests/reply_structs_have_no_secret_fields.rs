//! Source gate: no agent-facing reply struct may declare a
//! secret-typed field (ADR-024 §8).
//!
//! # The hole this fills
//!
//! `AgentSafeReply` is implemented on a struct as a whole and does
//! not recurse into its fields. It catches "someone added a new
//! reply type and forgot to mark it safe" — the audit fence lists
//! the types by name, so an unmarked one fails to compile.
//!
//! It does **not** catch "someone added a `value: SecretString`
//! field to a type that is already marked". That struct needs only
//! `Serialize`, the impl is already there, and the build stays
//! green. The `agent-trust-boundary` scenario claimed otherwise for
//! most of this epic's life.
//!
//! Making the trait recurse would need a derive macro, which is a
//! disproportionate amount of machinery for one invariant. Scanning
//! the declarations is cheap, catches the exact case, and — unlike
//! a comment asking people to be careful — fails.
//!
//! # What it can and cannot see
//!
//! It reads the source of the modules that declare agent-facing
//! replies and rejects a field whose type names a secret-carrying
//! type. It cannot see through a type alias, a generic parameter,
//! or a newtype wrapping a secret. Those are worth knowing about;
//! they are also considerably harder to write by accident than
//! `pub value: SecretString`.

use std::path::{Path, PathBuf};

/// Modules that declare replies an agent can receive.
///
/// Listed explicitly rather than scanned wholesale: the crate has
/// plenty of internal types that legitimately hold secrets, and a
/// gate that flags those would be turned off within a week.
const REPLY_MODULES: &[&str] = &[
    "src/secrets_tool.rs",
    "src/secrets_provision.rs",
    "src/secrets_validate.rs",
    "src/secrets_unlock.rs",
    "src/remediation.rs",
];

/// Type names that carry a secret value.
const SECRET_TYPES: &[&str] = &[
    "SecretString",
    "SecretBox",
    "SecretVec",
    "Zeroizing",
    "RecoveryPhrase",
];

fn crate_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// A field declaration that looks like it carries a secret.
#[derive(Debug)]
struct Offender {
    file: String,
    line: usize,
    text: String,
}

/// Scan one file for secret-typed fields inside struct bodies.
///
/// Deliberately simple: track whether we are inside a `struct`
/// block and flag `name: Type` lines whose type names something
/// from [`SECRET_TYPES`]. Function signatures and `use` lines are
/// skipped because they are not field declarations.
fn scan(path: &Path) -> Vec<Offender> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let file = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();

    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut in_struct = false;

    for (index, raw) in text.lines().enumerate() {
        let line = raw.trim();

        if !in_struct && (line.starts_with("pub struct ") || line.starts_with("struct ")) {
            // A unit or tuple struct on one line has no field
            // block to enter.
            if line.ends_with('{') || line.ends_with("{ ") {
                in_struct = true;
                depth = 1;
                continue;
            }
        }

        if in_struct {
            depth += line.matches('{').count();
            depth -= line.matches('}').count().min(depth);
            if depth == 0 {
                in_struct = false;
                continue;
            }

            // A field is `name: Type,` — skip attributes, comments
            // and anything that looks like a function.
            if line.starts_with('#') || line.starts_with("//") || line.contains("fn ") {
                continue;
            }
            if let Some((_, ty)) = line.split_once(':')
                && SECRET_TYPES.iter().any(|s| ty.contains(s))
            {
                out.push(Offender {
                    file: file.clone(),
                    line: index + 1,
                    text: line.to_owned(),
                });
            }
        }
    }
    out
}

/// The gate itself.
#[test]
fn no_agent_facing_reply_declares_a_secret_field() {
    let root = crate_root();
    let mut offenders = Vec::new();

    for module in REPLY_MODULES {
        offenders.extend(scan(&root.join(module)));
    }

    assert!(
        offenders.is_empty(),
        "{} field(s) in agent-facing reply modules carry a secret type:\n{}\n\nAn agent must \
         never receive a value. `AgentSafeReply` cannot catch this — it sits on the struct and \
         does not recurse into fields — which is why this gate exists. If the field is genuinely \
         not agent-facing, move it out of these modules rather than widening the gate.",
        offenders.len(),
        offenders
            .iter()
            .map(|o| format!("  {}:{} — {}", o.file, o.line, o.text))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// The gate must be able to see something, or it passes vacuously
/// forever and nobody notices.
#[test]
fn the_scanner_actually_reads_the_modules() {
    let root = crate_root();
    for module in REPLY_MODULES {
        let path = root.join(module);
        assert!(
            path.exists(),
            "{module} is listed as an agent-facing reply module but does not exist — the gate \
             would silently stop covering it"
        );
        let text = std::fs::read_to_string(&path).expect("readable");
        assert!(
            text.contains("struct"),
            "{module} declares no structs, so listing it here achieves nothing"
        );
    }
}

/// And it must actually flag a secret field when one is present.
///
/// Without this, a scanner bug would make the gate above pass on
/// anything — the failure mode of every source-scanning check.
#[test]
fn the_scanner_flags_a_secret_field() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("sample.rs");
    std::fs::write(
        &path,
        "#[derive(Serialize)]\n\
         pub struct SomeReply {\n    \
             pub path: String,\n    \
             /// This is the mistake the gate exists for.\n    \
             pub value: SecretString,\n\
         }\n",
    )
    .expect("write");

    let found = scan(&path);
    assert_eq!(
        found.len(),
        1,
        "the scanner missed a secret field: {found:?}"
    );
    assert!(found[0].text.contains("SecretString"));
}

/// ...and must not flag an ordinary field, or it will be disabled
/// the first time it cries wolf.
#[test]
fn the_scanner_leaves_ordinary_fields_alone() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("sample.rs");
    std::fs::write(
        &path,
        "pub struct SomeReply {\n    \
             pub path: String,\n    \
             pub expires_at: Option<String>,\n    \
             pub count: usize,\n\
         }\n\n\
         fn helper(secret: SecretString) -> String { String::new() }\n",
    )
    .expect("write");

    assert!(
        scan(&path).is_empty(),
        "the scanner flagged something that is not a reply field"
    );
}
