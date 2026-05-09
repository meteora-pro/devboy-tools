//! Encrypted local-vault primitives for `devboy-tools`.
//!
//! This crate is the implementation half of [ADR-023] section 3.1: the
//! on-disk format, the AEAD wrappers, the KDF, the envelope-encryption
//! layer, and the public [`Vault`] API.
//!
//! See [ADR-023](https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-023-secret-store-ux-layer.md)
//! for the design rationale, threat model, and file-format specification.
//!
//! Status: scaffolding — implementation lands in epic #247 phase P3.
