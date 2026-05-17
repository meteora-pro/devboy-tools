# devboy-vault-crypto

Encrypted local-vault primitives for `devboy-tools`. Implements the on-disk
format from [ADR-023] section 3.1: header, unlock envelopes (passphrase /
keychain / BIP39), per-entry XChaCha20-Poly1305 AEAD with `path` as
associated data.

Status: scaffolding. See epic
[#247](https://github.com/meteora-pro/devboy-tools/issues/247).

[ADR-023]: https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-023-secret-store-ux-layer.md
