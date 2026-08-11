//! "Agent never sees the value" trust-boundary plumbing per
//! [ADR-023] §3.7.
//!
//! The agent surface (every tool registered through
//! [`devboy_executor::tools::mcp_only_tools`] in the
//! `secrets_*` family) returns metadata only. Values live
//! exclusively in the daemon, the source plugins, and the
//! UI dialog — they never reach the JSON-RPC reply.
//!
//! This module makes that contract a typed thing, not just a
//! convention:
//!
//! 1. Reply structs for `secrets_*` tools opt into
//!    [`AgentSafeReply`].
//! 2. The trait is empty — implementing it is an explicit
//!    audit statement: "I have read every field of this struct
//!    and confirmed none of them carry a value the agent
//!    shouldn't see."
//! 3. A const-assertion at module load time forces every
//!    `AgentSafeReply` impl in this crate to compile through
//!    the same code path so a future refactor can't quietly
//!    drop the marker.
//!
//! ## Why this isn't auto-enforced via negative impls
//!
//! Rust's stable channel doesn't have negative trait impls or
//! `auto trait`, so we can't write
//! `impl !AgentSafeReply for SecretString {}`. The next-best
//! mitigation is the **CI grep gate** in
//! `tests/no_expose_secret_outside_allowlist.rs`: any new
//! `.expose_secret()` call in this crate fails the build,
//! with a small allowlist for vetted call sites.
//!
//! [ADR-023]: https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-023-secret-store-ux-layer.md

use serde::Serialize;

/// Marker trait for reply structs returned by `secrets_*` MCP
/// tools. Implementing it is an audit statement, not a
/// behavioural contract — the trait has no methods.
///
/// **Audit checklist** before implementing:
///
/// - No `SecretString` / `SecretBox<T>` fields, directly or
///   transitively.
/// - No `Vec<u8>` / `String` field that is filled from a
///   source plugin's `get()` or any `expose_secret()` site.
/// - No `serde(flatten)` / `serde(skip_serializing_if = ...)`
///   tricks that would let a value sneak in via a downstream
///   refactor.
///
/// `Serialize` is required because every reply ends up as
/// JSON-RPC text on the wire.
pub trait AgentSafeReply: Serialize {}

/// Compile-time fence: every type that should be returnable
/// from a `secrets_*` MCP tool must appear in this list. Adding
/// a reply struct here without an [`AgentSafeReply`] impl is a
/// type error, which is the entire point.
fn _audit_compile_fence() {
    fn assert_safe<T: AgentSafeReply>() {}
    assert_safe::<crate::secrets_tool::SecretsListItem>();
    assert_safe::<crate::secrets_tool::SecretsDescribeReply>();
    assert_safe::<crate::secrets_provision::ProvisionStatusReply>();
    assert_safe::<crate::secrets_provision::RequestIdReply>();
    // ADR-024 §8: remediation rides along on every error reply,
    // so it is subject to the same audit as the replies
    // themselves. It carries manifest metadata and fixed text —
    // never a value.
    assert_safe::<crate::remediation::Remediation>();
    assert_safe::<crate::secrets_validate::SecretsValidateReply>();
}
