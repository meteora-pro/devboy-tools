# devboy-storage

[![Crates.io](https://img.shields.io/crates/v/devboy-storage.svg)](https://crates.io/crates/devboy-storage)
[![Docs.rs](https://docs.rs/devboy-storage/badge.svg)](https://docs.rs/devboy-storage)
[![License](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](https://opensource.org/licenses/Apache-2.0)

Secure credential storage for [`devboy-tools`](https://github.com/meteora-pro/devboy-tools). Stores provider tokens (GitHub, GitLab, Jira, …) in the OS keychain, with environment variables as a CI fallback.

## Backends

| OS | Backend |
|---|---|
| macOS | macOS Keychain (`apple-native`) |
| Windows | Credential Manager (`windows-native`) |
| Linux | Secret Service over D-Bus (`sync-secret-service`, `crypto-rust`) |

Tokens are carried as `secrecy::SecretString` end-to-end (see [ADR-019](https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-019-secret-string-discipline.md)) — redacted on `Debug`, zeroized on drop.

## Add to your project

```toml
[dependencies]
devboy-storage = "0.26"
```

## Documentation

API reference on [docs.rs/devboy-storage](https://docs.rs/devboy-storage).

For the full bundle see the [main project README](https://github.com/meteora-pro/devboy-tools#readme).

## License

Apache-2.0 — see [LICENSE](https://github.com/meteora-pro/devboy-tools/blob/main/LICENSE).
