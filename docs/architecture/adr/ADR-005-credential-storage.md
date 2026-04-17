---
id: ADR-005
title: Credential storage — OS keychain with environment-variable fallback
status: accepted
date: 2026-01-13
deciders: ["Andrei Mazniak"]
tags: ["security", "credentials", "keychain", "cli"]
supersedes: null
superseded_by: null
---

# ADR-005: Credential storage

## Status

**accepted** — `devboy-storage` wraps the [`keyring`](https://docs.rs/keyring/) crate and is wired through `devboy-cli`. An environment-variable fallback chain is also shipped for CI/headless use.

## Context

`devboy-tools` needs to hold credentials for a pile of providers:

- GitLab / GitHub personal access tokens
- ClickUp / Jira API keys
- Slack / Telegram bot tokens
- Proxy MCP server tokens (for `devboy init --proxy`)
- Remote config endpoint tokens (for `devboy init --remote-config-url`)

Requirements:

1. **Security** — tokens must not sit in plaintext config files
2. **Cross-platform** — macOS, Linux, and Windows
3. **Fully local** — the OSS binary must never require a network service just to read a credential
4. **Headless / CI support** — containers, `cron` jobs, and CI runners typically can't reach a keychain daemon; we need a fallback that doesn't punish those environments
5. **UX** — initial setup should be a single interactive flow (`devboy init`) that stores the token in the right place

## Decision

Use the platform-native **OS keychain** as the primary secret store, and expose an **environment-variable fallback chain** for environments that can't reach the keychain.

### Architecture

```
┌──────────────────────────────────────────────────────────────┐
│                   devboy-storage crate                        │
├──────────────────────────────────────────────────────────────┤
│                                                               │
│  CredentialStore (trait)                                      │
│       │                                                       │
│       ├── KeychainStore      → OS keychain via `keyring`      │
│       ├── EnvVarStore        → process environment variables  │
│       └── ChainStore         → prefer env vars, then keychain │
│                                                               │
└──────────────────────────────────────────────────────────────┘
```

### Keychain backends per platform

| Platform | Backend |
|----------|---------|
| macOS | Keychain Services |
| Windows | Credential Manager |
| Linux (desktop) | Secret Service (GNOME Keyring / KWallet) via D-Bus |
| Linux (headless) | No keychain daemon — use the env-var fallback |

### Key naming convention

The keychain **service name** is a fixed constant: `devboy-tools`.

The keychain **account/key** follows a dot-separated `<namespace>.<credential_name>` convention:

```
gitlab.token
github.token
clickup.token
jira.token
jira.email                    # for Basic Auth providers
proxy.<name>.token            # from `devboy init --proxy`
remote_config.token           # from `devboy init --remote-config-url`
```

The dot separator matches the TOML config style (`[gitlab] token = "..."`), making the mapping between config references and stored secrets obvious.

### Environment-variable fallback

For CI, Docker, and headless Linux hosts, the resolver checks environment variables before falling back to the keychain:

```
DEVBOY_{PROVIDER}_TOKEN          # preferred prefixed form
  └── fallback to
{PROVIDER}_TOKEN                 # unprefixed, for compatibility with other tools
  └── fallback to
OS keychain (service: devboy-tools, key: <provider>.token)
```

Example for GitHub:

```bash
# Preferred
export DEVBOY_GITHUB_TOKEN=ghp_xxx

# Also accepted (compatible with gh CLI, actions, etc.)
export GITHUB_TOKEN=ghp_xxx
```

This order lets CI jobs configure tokens the same way they do for other tools while giving `devboy`-specific env vars precedence when both are set.

### Local config shape (tokens are **not** stored here)

```toml
# .devboy.toml or ~/.devboy/config.toml
# Tokens live in the keychain / env vars. Only references live here.

[contexts.my-project.github]
owner = "meteora-pro"
repo = "devboy-tools"
# token: read from keychain service `devboy-tools`, key `github.token`
# or from env var DEVBOY_GITHUB_TOKEN / GITHUB_TOKEN

[contexts.my-project.gitlab]
url = "https://gitlab.com"
project_id = "meteora-pro/devboy-tools"
```

### `CredentialStore` trait

The trait is intentionally synchronous and single-keyed — credential access is not in the hot path of any async workload, and keying by a single string matches the TOML convention.

```rust
// crates/devboy-storage/src/lib.rs
pub trait CredentialStore: Send + Sync {
    fn store(&self, key: &str, value: &str) -> Result<()>;
    fn get(&self, key: &str) -> Result<Option<String>>;
    fn delete(&self, key: &str) -> Result<()>;

    fn exists(&self, key: &str) -> bool { matches!(self.get(key), Ok(Some(_))) }

    /// Reports whether the backend is reachable (e.g. keychain can be opened
    /// in a CI / headless container).
    fn is_available(&self) -> bool { true }

    /// Reports whether the backend accepts writes (e.g. env-var stores are
    /// read-only).
    fn is_writable(&self) -> bool { true }
}
```

Concrete implementations:

- **`KeychainStore`** — backed by [`keyring`](https://docs.rs/keyring/)
- **`EnvVarStore`** — read-only; resolves keys through the env-var fallback chain described above
- **`ChainStore`** — composes multiple backends in priority order. `ChainStore::default_chain()` gives you "env vars → keychain".

### Interactive setup (`devboy init`)

`devboy init` collects tokens through a wizard (or via CLI flags like `--remote-config-token` for non-interactive use) and persists them via `CredentialStore::store`, which picks the right backend — keychain if available, else a clear error with instructions to use env vars instead.

## Consequences

### Positive

- ✅ **No plaintext tokens on disk** in normal operation
- ✅ **Cross-platform** — `keyring` hides the OS differences
- ✅ **Works in CI / headless containers** via the env-var fallback, without requiring a running keychain daemon
- ✅ **Standard env var names** — compatible with `gh` CLI, `glab`, and ecosystem tools
- ✅ **Single setup flow** — `devboy init` handles all providers

### Negative

- ❌ **Platform-dependent behaviour** — minor quirks between macOS Keychain, Windows Credential Manager, and Secret Service
- ❌ **No sync across machines** — by design; credentials are per-host
- ❌ **No backup** — if the user wipes their keychain, they must re-run setup

### Platform-specific notes

| Platform | Backend | Caveats |
|----------|---------|---------|
| macOS | Keychain Services | First access may prompt for user permission |
| Windows | Credential Manager | Built-in, reliable |
| Linux (desktop) | Secret Service / KWallet | Requires D-Bus; auto-unlocks only when the session keyring is unlocked |
| Linux (headless/CI) | — | Use env vars; keychain calls will fail with a recoverable error |

## Alternatives Considered

### Alternative 1: Encrypted file at `~/.devboy/credentials.enc`

**Why rejected:** Requires the user to type a master password on every invocation, which makes non-interactive use (agents, scripts) painful. Less secure than the OS keychain, which integrates with the OS login session.

### Alternative 2: Environment variables only

**Why rejected:** Inconvenient for interactive developer use. No persistence between shell sessions. Setup wizard becomes awkward because the user has to know how to persist env vars in their shell profile.

### Alternative 3: HashiCorp Vault (or similar)

**Why rejected:** Overkill for a local CLI tool. Requires running a separate service. The OS keychain covers the single-user case already.

## Implementation

- **Crate:** `crates/devboy-storage/` — single-file today (`src/lib.rs`) housing the `CredentialStore` trait and the `KeychainStore` / `EnvVarStore` / `ChainStore` backends
- **Service name:** `devboy-tools` (constant in `src/lib.rs`)
- **Key naming:** `<provider>.<credential_name>` (e.g. `github.token`, `proxy.<name>.token`, `remote_config.token`)
- **CLI integration:** `crates/devboy-cli/src/main.rs` (the `Init` / `Config` subcommands drive token collection and storage)
- **Docs:** `README.md` → "Alternative: Environment Variables (CI/CD)" section

## References

- [`keyring` crate](https://docs.rs/keyring/)
- [macOS Keychain Services](https://developer.apple.com/documentation/security/keychain_services)
- [Windows Credential Manager](https://learn.microsoft.com/en-us/windows/win32/api/wincred/)
- [freedesktop.org Secret Service](https://specifications.freedesktop.org/secret-service/)
- [ADR-002: Rust-based architecture](./ADR-002-rust-architecture.md)

---

## Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-01-13 | Andrei Mazniak | Initial version |
| 2026-04-17 | Andrei Mazniak | Translated to English; documented the env-var fallback chain; marked accepted |
| 2026-04-17 | Andrei Mazniak | Synced with shipped `devboy-storage`: sync (not async) trait with single-key `store`/`get`/`delete`/`exists`/`is_available`/`is_writable`; service name `devboy-tools`; dot-separated keys (`github.token`, `proxy.<name>.token`, `remote_config.token`) |
| 2026-04-17 | Andrei Mazniak | Fixed stale `CredentialStore::set` → `::store` in the setup wizard section; updated the Implementation section to match the actual single-file `crates/devboy-storage/src/lib.rs` and `crates/devboy-cli/src/main.rs` layout |
| 2026-04-17 | Andrei Mazniak | Final sweep of keychain key formatting: env-var fallback chain now shows `OS keychain (service: devboy-tools, key: <provider>.token)`; the local-config TOML example refers to `keychain service devboy-tools, key github.token` instead of the old slash-separated form |
