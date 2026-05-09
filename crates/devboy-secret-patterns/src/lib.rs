//! Pattern catalogue for `devboy-tools` secrets.
//!
//! Shared with the OTLP sanitizer and the `otel scan` auditor — one source
//! of truth for "what does a GitHub PAT look like, where do you get a fresh
//! one, how often should it be rotated".
//!
//! See [ADR-023] §3.6 for the trait shape and inheritance rules.
//!
//! [ADR-023]: https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-023-secret-store-ux-layer.md
//!
//! Status: scaffolding — implementation lands in epic #247 phase P2.
