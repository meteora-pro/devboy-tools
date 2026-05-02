---
id: ADR-019
title: Secrets carry SecretString end-to-end
status: accepted
date: 2026-05-02
deciders: ["meteora-pro/devboy-tools maintainers"]
tags: ["security", "core", "storage", "providers"]
supersedes: null
superseded_by: null
---

# ADR-019: Secrets carry SecretString end-to-end

## Status

**accepted**

## Context

Plain `String` for a secret leaks through several routes that no amount of
discipline at the call site can fully close:

- **`Debug` derives.** A stray `tracing::debug!("client = {:?}", client)` puts
  the token in the log. Several provider clients used `derive(Debug)` on
  structs whose token field was just `String`.
- **Memory dumps and core files.** Plain `String` heap allocations stay live
  until the allocator overwrites them. `SecretBox` (the type behind
  `SecretString`) calls `Zeroize::zeroize` on `Drop`.
- **Snapshot or fixture serialization.** `serde_json::to_string(&config)`
  happily emits the plaintext. Several test fixtures and a few real call
  paths went through `serde_json` round-trips with secrets in the struct.
- **`.clone()` proliferation.** Each clone is another copy of the secret on
  the heap. Wrapping the secret prompts the question "should I really be
  cloning this here?" instead of doing it implicitly.

The previous discipline relied on `#[serde(skip_serializing)]` on individual
fields and reviewer attention. That worked but is fragile: a new contributor
adding a struct that "just holds a token" doesn't necessarily know the rule.
Issue #225 was filed after PR #212 review surfaced one such drift in the
Confluence client.

## Decision

> **Decision:** Every secret in `crates/` is carried as
> `secrecy::SecretString` (or a wrapper that redacts on `Debug` and zeroizes
> on `Drop`) end-to-end — from `CredentialStore::get` to the HTTP call site.
> The plaintext is exposed only through `.expose_secret()` at the smallest
> possible scope.

Specifically:

- **Storage** — `CredentialStore::get` returns `Option<SecretString>`,
  `CredentialStore::store` accepts `&SecretString`. The keychain wrapper, the
  env-var wrapper, the in-memory cache (`CachedStore`) and `ChainStore` all
  preserve the type end-to-end.
- **Provider clients** — every `Client::new` constructor takes the token by
  value as `SecretString`. The internal field is `SecretString`. Auth
  headers / `bearer_auth` / `basic_auth` calls invoke `.expose_secret()`
  inline; no helper holds the plaintext.
- **Executor context** — `ProviderConfig` variants store
  `access_token: SecretString`, `api_key: SecretString`,
  `password: SecretString`. `ConfluenceAuthConfig::{BearerToken, Basic}`
  carry `SecretString` for token / password.
- **MCP proxy** — `McpProxyClient::connect` takes `Option<&SecretString>`.
- **Telemetry** — `TelemetryAuth::bearer_token` is `Option<SecretString>`.
- **`AdditionalContext` / `ProviderConfig`** — these structs intentionally
  drop their previous `Serialize` / `Deserialize` derives. They carry
  plaintext access tokens; serializing them to JSON would defeat the
  discipline. Construct provider configs in-process from `Config` plus
  `CredentialStore`, never round-trip through a transport.
- **`SentryConfig.dsn`** — kept as `Option<String>` because the on-disk TOML
  config must round-trip the value. The `Debug` impl is hand-written to
  redact the DSN; the userinfo segment of a Sentry DSN is the auth token.

A CI gate (`secrets-discipline` job in `.github/workflows/ci.yml`) greps for
`(token|api_key|password|secret|client_secret|access_token|refresh_token|bearer_token):\s*(Option<)?\s*String`
in non-test source under `crates/` and fails the build if any match exists.
The CLI binary `crates/devboy-cli/src/main.rs` is exempted because clap value
parsers operate on `String` at the user-input boundary; that string is
wrapped in `SecretString` immediately on use and never persisted as a
plaintext field. Tests are exempted because their fixtures pre-wrap the
literal at the field boundary.

## Consequences

### Positive

- ✅ `Debug` of every config struct redacts secrets — every previous
  hand-rolled `Debug` in provider clients now collapses into the standard
  `derive(Debug)` because the inner `SecretString` does the right thing.
- ✅ `serde_json::to_string` of a `ProviderConfig` is a compile error
  (no `Serialize` impl) instead of a silent leak through the wire format.
- ✅ Heap buffers holding tokens are zeroized on drop — meaningful for
  memory-dump and core-file forensics.
- ✅ Clone is explicit (`access_token.clone()` in `factory.rs`) — every
  duplicate of the secret is visible in code review.
- ✅ A single CI grep gate prevents drift; the rule is enforced rather than
  remembered.

### Negative

- ❌ One ~50 KB dependency (`secrecy = "0.10"`) added to the workspace.
- ❌ One `.expose_secret()` call at every HTTP call site (6 provider
  clients, MCP proxy, telemetry uploader, Sentry DSN init).
- ❌ `ProviderConfig` and `AdditionalContext` lost their `Serialize` /
  `Deserialize` impls. If a future feature needs to serialize them across
  process boundaries, a wrapper that explicitly opts in to serialization
  has to be introduced (and reviewed against this ADR).

### Risks

- ⚠️ **Drift via new struct.** A contributor adds `MyProviderClient` with
  `token: String`, the CI grep gate catches it.
- ⚠️ **`.expose_secret()` in the wrong place.** The token must not be
  copied into a `String` for logging or constructing intermediate values.
  Mitigation: `.expose_secret()` returns `&str`, so the natural usage is
  `format!("Bearer {}", token.expose_secret())` inline at the call site.
  Code review checks for `expose_secret().to_string()` patterns — those
  are usually a smell.
- ⚠️ **TOML config still stores DSN in plaintext.** `SentryConfig.dsn`
  stays `Option<String>` because `secrecy::SecretBox<str>` does not
  implement `serde::Serialize` (the inner `str` is unsized and is not
  `SerializableSecret`). The hand-written `Debug` impl redacts it, but
  on-disk config files still contain the DSN literally — that's the same
  exposure surface as any other dotfile under `~/.devboy/`.

## Alternatives Considered

### Alternative 1: Keep `String`, document the convention

**Description:** Stay with plain `String` and rely on
`#[serde(skip_serializing)]` plus reviewer attention to keep tokens out of
logs and serialized output.

**Why rejected:** This was the prior state. PR #212 review showed it had
silently regressed in several places. The discipline lasts only as long as
every reviewer remembers it on every PR.

### Alternative 2: Custom newtype that implements `SerializableSecret`

**Description:** Define `SerializableSecretString` as a wrapper around
`SecretBox<SecretInner>` where `SecretInner: SerializableSecret`. This would
let `ProviderConfig` keep `Serialize` / `Deserialize` and still carry the
secret type-safely.

**Why rejected:** Cross-process serialization of `ProviderConfig` is not a
production code path today (only test fixtures used to round-trip it).
Adding the wrapper would create a parallel "secret-but-serializable" type
that downstream code would copy from instead of reaching for the standard
`SecretString`. If the need ever arises, the wrapper can be introduced as a
separate decision; right now removing the unused `Serialize` derive is
cleaner.

### Alternative 3: Use `zeroize::Zeroizing<String>` directly

**Description:** Avoid the `secrecy` dependency entirely; use
`Zeroizing<String>` so the heap buffer is wiped on drop, and rely on
discipline to keep `Debug` clean.

**Why rejected:** `Zeroizing<String>` does **not** redact `Debug` — it
prints the inner value verbatim. The Debug-redaction property is the more
valuable half of the discipline; losing it for a smaller dependency
surface area is the wrong trade.

## Implementation

- **Issues:** [#225](https://github.com/meteora-pro/devboy-tools/issues/225)
- **PR:** to be filed against `meteora-pro/devboy-tools`
- **Code:**
  - `crates/devboy-storage/src/lib.rs` — `CredentialStore` trait
  - `crates/devboy-storage/src/cache.rs` — `CachedStore`
  - `crates/devboy-executor/src/context.rs` — `ProviderConfig`,
    `ConfluenceAuthConfig`, `AdditionalContext`
  - `crates/plugins/api/{github,gitlab,clickup,jira,confluence,fireflies,slack}/src/client.rs`
  - `crates/devboy-mcp/src/proxy.rs` — `McpProxyClient::connect`
  - `crates/devboy-mcp/src/telemetry.rs` — `TelemetryAuth`
  - `crates/devboy-core/src/config.rs` — `SentryConfig` Debug impl
  - `crates/llm-eval/src/main.rs` — `ModelConfig.api_key`
  - `.github/workflows/ci.yml` — `secrets-discipline` job

## References

- [`secrecy` crate documentation](https://docs.rs/secrecy/)
- [`zeroize` crate documentation](https://docs.rs/zeroize/)
- [Original PR #212 review comment](https://github.com/meteora-pro/devboy-tools/pull/212#discussion_r3172347974) — the trigger for issue #225
- [ADR-005: Credential storage](./ADR-005-credential-storage.md) — describes
  where secrets *live* (keychain + env vars); this ADR describes how
  secrets are *typed in transit* through the process

---

## Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-05-02 | meteora-pro/devboy-tools | Initial version |
