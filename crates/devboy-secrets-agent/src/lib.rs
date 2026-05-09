//! `devboy-secrets-agent` — long-running daemon that owns the unlocked
//! local vault and exposes it through a JSON-RPC 2.0 wire over a UNIX
//! domain socket.
//!
//! See [ADR-023] §3.3 for the lifecycle, wire protocol, and security
//! boundary.
//!
//! [ADR-023]: https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-023-secret-store-ux-layer.md
//!
//! Status: scaffolding — implementation lands in epic #247 phase P4.
