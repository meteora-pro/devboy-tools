# devboy-secrets-agent

Long-running daemon that holds the unlocked `local-vault` key and serves it to
the CLI / UI over JSON-RPC on a UNIX socket. See
[ADR-023](https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-023-secret-store-ux-layer.md)
§3.3.

Status: scaffolding. See epic
[#247](https://github.com/meteora-pro/devboy-tools/issues/247).
