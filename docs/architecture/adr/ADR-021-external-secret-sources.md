---
id: ADR-021
title: External secret sources and backend routing
status: proposed
date: 2026-05-06
deciders: ["Andrei Mazniak"]
tags: ["security", "secrets", "plugins", "storage"]
supersedes: null
superseded_by: null
---

# ADR-021: External secret sources and backend routing

## Status

**proposed**

## Context

[ADR-005](./ADR-005-credential-storage.md) ships a single backend layout:
the OS keychain on the user's machine, with an environment-variable
fallback for CI and headless hosts. [ADR-020](./ADR-020-secret-manifest-and-alias-resolution.md)
adds a manifest, a path namespace, and an alias-resolution layer above
the credential store, but it intentionally does not say where values
come from beyond what ADR-005 already provided.

Real-world deployments rarely live on one backend:

- **Teams running HashiCorp Vault** keep credentials there for audit,
  rotation, and per-team policy. They do not want a copy of every
  secret in every developer's local keychain.
- **Teams using 1Password** treat the 1Password vault as the source
  of truth, with the CLI (`op`) and biometric session as the access
  path. Local keychain entries become stale almost immediately.
- **CI runners and bare-Linux containers** typically have no D-Bus,
  no Secret Service daemon, no `op`, and sometimes not even a TTY.
  The only shape that works is environment variables, possibly
  loaded from a file mounted by the orchestrator (Docker secrets,
  Kubernetes secret mounts).
- **A single user routinely talks to several upstreams of the same
  type.** Two Vault servers under different addresses, two 1Password
  accounts (work and personal), two AWS profiles. Each carries its
  own credentials and possibly its own role.
- **The same upstream may need different credentials in different
  contexts.** An engineer may hold `read-only` and `deploy` tokens
  for the same Vault and want a context switch to flip which one is
  active, without re-authenticating.

ADR-020 left this surface deliberately empty. This ADR fills it.

## Decision

> **Decision:** The credential store is split into a thin **router**
> and a set of **secret-source plugins**. A secret path declared
> through the ADR-020 manifest resolves through the router to exactly
> one source, which in turn knows how to talk to its upstream
> (keychain, 1Password CLI, HashiCorp Vault, an env-store backend, or
> a community-supplied subprocess plugin). The router is the only
> code that touches the manifest; the sources are interchangeable.

The decision has eight parts.

### 1. The `SecretSource` trait

A source is any backend able to answer questions about secrets. The
trait is small and explicitly capability-aware:

```rust
#[async_trait]
pub trait SecretSource: Send + Sync {
    fn name(&self) -> &str;
    fn capabilities(&self) -> Capabilities;     // READ | LIST | VALIDATE | WRITE | ROTATE

    async fn is_available(&self) -> SourceStatus;        // Available | Locked | NotInstalled | Error
    async fn get(&self, reference: &str) -> Result<Option<SecretString>>;
    async fn list(&self) -> Result<Vec<RemoteRef>>;       // optional, used by discovery
    async fn validate(&self, reference: &str) -> Result<()>;
}
```

`reference` is a backend-specific string — for example
`op://Personal/GitHub PAT/credential` for 1Password,
`secret/data/team/gitlab#token` for Vault KV v2, a flat key for the
keychain, an environment-variable name for the env-store. Sources do
**not** know about ADR-020 paths; mapping a path to a reference is the
router's job (section 2). This separation lets a source plugin be
written without any awareness of the manifest layer.

`capabilities()` lets the system reason about what a source can and
cannot do. A read-only source (1Password CLI in the typical biometric
configuration) declares `READ | LIST | VALIDATE`; an env-store
declares `READ` only; a Vault KV v2 source with sufficient policy may
declare `READ | LIST | VALIDATE | WRITE | ROTATE`. Operations that
require a missing capability fail with a structured error rather than
trying and erroring at the network boundary.

### 2. Routing (`~/.devboy/secrets/sources.toml`)

Routing maps an ADR-020 path to a `(source, reference)` pair. The
configuration is global and lives at `~/.devboy/secrets/sources.toml`:

```toml
# Source definitions

[[source]]
name = "keychain"
type = "keychain"

[[source]]
name = "1p-personal"
type = "1password"
account = "personal.example.1password.com"

[[source]]
name = "vault-team"
type = "vault"
addr   = "https://vault.example.internal/"
mount  = "secret"

[[source]]
name = "env-store"
type = "env-store"
file = "/run/secrets/devboy.env"   # optional; loaded if present

# Routing — prefix-based default

[default]
source = "keychain"

[[route]]
prefix = "team/"
source = "vault-team"
mount  = "secret/data/team"        # path → secret/data/team/<rest-of-path>

[[route]]
prefix = "personal/"
source = "1p-personal"
vault  = "Personal"                # path → op://Personal/<rest-of-path>

# Per-secret override (rare; for one-off mappings)

[secret."client-acme/jira/api-key"]
source    = "1p-personal"
reference = "op://Work/Acme Jira/credential"
```

The router resolves a path by:

1. Looking for a `[secret."<path>"]` block — if present, the explicit
   source and reference win.
2. Otherwise, finding the longest matching `[[route]]` prefix.
3. Otherwise, falling back to `[default]`.

Path-to-reference mapping for prefix routes is source-specific and
defined by the source plugin (the keychain source uses the path
directly; the Vault source joins `mount` with the path tail; the
1Password source produces an `op://` URL from the `vault` setting and
the path tail).

### 3. Multiple instances of the same source type

Each `[[source]]` entry has a unique `name`. Two `vault` sources with
different `addr` values are two distinct sources, addressable
independently from routing. The `type` is what code is invoked; the
`name` is how users and routes refer to the source. This is the
mechanism by which a single user can talk to several upstreams of the
same flavour without confusion.

### 4. Per-context source credentials

A source may itself need credentials to operate (Vault token,
1Password account session, AWS profile). The default behaviour is
the source's native session management — `op signin`, `vault login`,
`aws configure sso`. ADR-005's keychain remains the place for tokens
that the system itself manages.

For users who hold several roles in the same upstream — for example,
a `read-only` token and a `deploy` token for the same Vault — the
**active context** of `devboy-tools` may select which credential
profile to use. The context configuration gains a new optional block:

```toml
# in the active context

[source_credentials]
"vault-team"  = "__sources/vault-team/deploy"   # path under reserved namespace
"1p-personal" = "biometric"                      # source-defined sentinel: "use system unlock"
```

Switching contexts therefore switches credentials at the source
level, without re-authenticating each time. The mapping value is
either:

- A path under the reserved `__sources/` namespace (section 5), in
  which case the router fetches it from the credential store and
  hands it to the source's `init` step; or
- A source-specific sentinel string (`biometric`, `default-profile`)
  that the source plugin interprets as "use your native unlock
  mechanism".

### 5. Reserved `__sources/` namespace

ADR-020 reserves the `__*` prefix as internal. This ADR uses
`__sources/<source-name>/<profile>` for source-authentication
credentials.

```
__sources/vault-team/deploy
__sources/vault-team/read-only
__sources/aws-shared/sso-session
```

These paths are valid in the credential store but are filtered out of
`devboy secrets list` by default. They surface only with
`devboy secrets list --internal`. The reasoning: source credentials
are infrastructure, not business credentials, and showing them next
to user-facing secrets clutters the discovery view.

### 6. Subprocess plugin protocol

Built-in sources (keychain, 1password, vault, env-store; section 8)
implement `SecretSource` directly inside the `devboy-tools` binary.
Community backends ship as separate executables, discovered at
`~/.devboy/plugins/secrets/devboy-source-<name>`, communicating with
the router over **JSON-RPC on stdio** — the same shape as MCP.

The protocol surfaces the trait directly:

```
secret_source.init        →  capabilities, version
secret_source.is_available →  status enum
secret_source.get         →  reference → SecretString | NotFound
secret_source.list        →  → RemoteRef[]
secret_source.validate    →  reference → ok | invalid | unreachable
```

The router spawns the plugin lazily (first use), keeps the process
alive for a configurable idle window (default sixty seconds), and
restarts it transparently on crash. The plugin is run with a
restricted environment: only variables it explicitly declares it
needs in a manifest shipped alongside the binary
(`devboy-source-<name>.toml`). The router never passes other secrets
to a plugin.

The protocol is part of this ADR so that built-in sources and
external sources are interchangeable from the start. The first
release ships only built-ins; the protocol exists so a community
crate (AWS Secrets Manager, Bitwarden, Doppler, Infisical, …) can be
written without forking `devboy-tools`.

### 7. Caching

Source latency varies by orders of magnitude. A keychain read is
microseconds; a `vault read` is tens of milliseconds plus a TLS
handshake; an `op read` typically takes hundreds of milliseconds and
may prompt for biometric unlock; a misconfigured Vault behind a
captive-portal VPN may hang for seconds. Without caching, an agent
that resolves a dozen secrets per minute is unusable.

The router caches resolved values **in-process, in memory only,
never on disk**. The cache is keyed by ADR-020 path and holds an
expiring `SecretString`. The default TTL is fifteen minutes; it is
configurable per source and per secret. Cache entries are dropped
when:

- the TTL expires,
- the user invokes `devboy secrets refresh <path>` (or `--all`),
- a source declares an out-of-band invalidation (Vault lease
  revocation, 1Password session timeout — surfaced through
  `is_available()`).

The cache is **never** persisted. Process exit drops every entry.
This is the same posture as `secrecy::SecretString::zeroize_on_drop`
extended one level up.

### 8. Built-in sources

The first release ships four built-in implementations of
`SecretSource`. Together they cover the dominant deployment shapes
without requiring any community plugin.

#### `keychain`

A refactor of the existing `KeychainStore` from ADR-005, wrapped in
the `SecretSource` interface. `reference` is the path itself — the
keychain is the only source where the ADR-020 path doubles as the
backend reference. Capabilities: `READ | LIST | VALIDATE | WRITE`.
Available unless the platform exposes no keychain (headless Linux
without Secret Service); in that case `is_available()` returns
`NotInstalled` and the router skips it.

#### `1password`

Backed by the `op` CLI. `reference` is an `op://` URL
(`op://<vault>/<item>/<field>`). Capabilities: `READ | LIST |
VALIDATE`. Write capability is omitted from the first release;
`op item create` is mudded enough that the ADR-020 `bootstrap` flow
opens the 1Password UI for new entries instead of writing through the
CLI. `is_available()` checks `op whoami`; a locked session returns
`Locked` and `doctor` shows an actionable message
(`op signin`).

#### `vault`

Backed by the HashiCorp Vault HTTP API, KV v2. `reference` joins the
mount and path (`secret/data/<path>#<field>`, where `<field>` defaults
to `value`). Capabilities: `READ | LIST | VALIDATE | WRITE | ROTATE`,
subject to the policy associated with the active token.
Authentication methods are pluggable; the ADR ships token, AppRole,
and OIDC out of the box, with the active method configured per
source instance. `is_available()` calls `/sys/health` and a token
lookup; an expired token returns `Locked`.

#### `env-store`

A first-class source for CI, containers, and bare Linux. There is no
keychain to fall back to in those environments; treating env-vars as
their own source makes the routing explicit and the failure modes
clean.

The env-store reads values through three mechanisms, in order:

1. A **manifest-defined alias** for the secret. The ADR-020 global
   index may declare `env_var = "GITLAB_TOKEN_DEPLOY"` for a path,
   in which case the env-store reads that variable verbatim. This
   exists for compatibility with existing CI conventions, where
   variables already have well-known names that pre-date
   `devboy-tools`.
2. The **convention-based name** derived from the path:
   `DEVBOY_SECRET__<flattened-path>`, with `/` rewritten to `__` and
   `-` to `_`, uppercased.

   ```
   team/gitlab/token-deploy
   → DEVBOY_SECRET__TEAM__GITLAB__TOKEN_DEPLOY
   ```

3. The **file loader**: at startup, if `DEVBOY_SECRETS_FILE` points
   at an existing file, it is parsed as a `.env`-style file and its
   contents are merged into the process environment. This is the
   mechanism that picks up Docker secrets and Kubernetes secret
   mounts.

Capabilities: `READ` only. Validation is format-only; liveness is
delegated to the higher-level provider check.

#### CI auto-detection

When the process detects that it is running in CI — environment
variables `CI`, `GITLAB_CI`, `GITHUB_ACTIONS`, or
`BUILDKITE` set to a truthy value — the router silently promotes
`env-store` to the first position in the resolution chain and skips
sources whose `is_available()` returns `NotInstalled`. This
preserves zero-config CI behaviour while keeping local developer
defaults unchanged.

### 9. `doctor` integration

`devboy doctor` gains two new sections:

- **Sources** — for each configured source, its name, type,
  availability status, last successful contact, and any actionable
  message (`op signin`, `vault login`, `unset DEVBOY_SECRETS_FILE`,
  …).
- **Secrets in active context** — for each path declared in the
  active manifest, its routed source, current status (provisioned /
  expiring / missing / format-invalid), and (where the upstream
  exposes it) its expiry. Optional secrets are shown in their own
  sub-section without producing a non-zero exit.

Exit code is non-zero if any required secret is missing or
format-invalid. This makes `devboy doctor` a usable CI sanity gate
without further wrapping.

## Threat model adjustments

ADR-020 stated the framework's overall threat model. This ADR moves
two things on the spectrum:

- **Improvement.** External sources typically provide audit (every
  read is logged in 1Password and Vault), centralized rotation, and
  per-team policy. A team that adopts an external source
  materially raises the bar over a per-developer keychain — not
  because `devboy-tools` is more secure, but because the upstream
  is.
- **New surface.** `devboy-tools` invokes `op` and `vault` (and
  community plugins) as subprocesses. A compromised CLI on the
  user's `PATH` can return arbitrary values, log credentials, or
  exfiltrate the entire vault. **Mitigation:** sources are spawned
  with explicit absolute paths resolved at install/configure time
  and pinned in the source definition; checksums of built-in source
  binaries are verified by `doctor`. The router never invokes
  source CLIs through a shell — `Command::new(absolute_path)` with
  `argv` only.

Subprocess plugins inherit this surface and add another layer
(arbitrary code in the user's plugin directory). Plugin manifests
must be reviewed before install; `devboy secrets sources install
<plugin>` will (in the implementation) display the plugin's declared
permissions and require explicit confirmation.

## Consequences

### Positive

- ✅ **Adoption path for existing teams.** Teams already on
  1Password or Vault can route their existing tree into the
  ADR-020 namespace without copying a single value into the local
  keychain.
- ✅ **CI works first-class.** The env-store is a real source, not
  a fallback; CI behaviour is therefore predictable and
  configurable rather than implicit.
- ✅ **Multiple upstreams of the same type are addressable.** A user
  with several Vault servers or several 1Password accounts can
  configure them side by side under different `name`s.
- ✅ **Context-aware role switching.** A context switch flips
  which credential is presented to a source — a `read-only` token
  in one context, a `deploy` token in another.
- ✅ **A community plugin protocol exists from day one.** A new
  backend is a separate binary against a documented stdio protocol;
  it does not require forking `devboy-tools`.
- ✅ **Cached reads keep agents responsive.** A fifteen-minute TTL
  is invisible to interactive use and makes biometric-prompting
  sources usable inside agentic loops.

### Negative

- ❌ **One more configuration file.** `~/.devboy/secrets/sources.toml`
  joins the existing `~/.devboy/config.toml` and the ADR-020
  `~/.devboy/secrets/index.toml`.
- ❌ **Built-in sources couple the binary to upstream CLI versions.**
  A breaking change in `op` or `vault` may surface as a new
  validation failure. **Mitigation:** integration tests in CI run
  against pinned versions of the upstream CLIs.
- ❌ **Subprocess plugins double the install surface for users who
  want non-built-in backends.** Discovery, signing, and update of
  plugins is itself a problem; this ADR ships the protocol but
  defers a managed plugin registry to a later decision.

### Risks

- ⚠️ **Cache outliving an upstream rotation.** A secret rotated in
  Vault is still served from the in-process cache for up to fifteen
  minutes. **Mitigation:** the upper bound is bounded; sources
  expose `validate()` so a misbehaving downstream can be told to
  flush; `secrets refresh` is one command.
- ⚠️ **Misconfigured route silently routes to the wrong upstream.**
  A typo in a `prefix` lands a `team/...` path in the keychain
  instead of Vault. **Mitigation:** `devboy doctor` shows the
  resolved source for every required path; a route that resolves
  to the default source for a non-default prefix is flagged.
- ⚠️ **A compromised source CLI in PATH.** Covered in the threat
  model above; the mitigation is absolute paths plus checksums.
- ⚠️ **Subprocess plugin lifetime leaks.** A plugin that fails to
  exit cleanly leaves zombies. **Mitigation:** the router enforces
  a kill timeout (default ten seconds after idle) and reports
  zombies in `doctor`.

## Alternatives Considered

### Alternative 1: Single static backend, configured globally

**Description:** Pick one backend per machine (keychain *or* Vault
*or* 1Password) and configure the whole tree to live there.

**Why rejected:** Real users hold mixes — a personal 1Password vault
plus a team Vault plus a CI env-store. A single-backend model forces
either duplication (copy team secrets into 1Password as well) or a
manual ladder of fallbacks. The routing layer is the price of
honest support.

### Alternative 2: Use environment variables only outside the keychain

**Description:** Treat external sources as a CI-only concern and
keep the local-development story keychain-only.

**Why rejected:** This is the status quo of ADR-005; teams already
on 1Password or Vault either bypass `devboy-tools` or keep stale
copies of credentials in two places. The point of this ADR is
exactly to make 1Password and Vault first-class on the developer
machine.

### Alternative 3: Dynamic linking / WASM plugin model

**Description:** Load source plugins as `.so` / `.dylib` files via
`libloading`, or as WASM modules.

**Why rejected:** Both options raise the floor for plugin authors
(cross-compilation, ABI stability, sandbox escape stories) and
deliver no advantage over a subprocess. Stdio JSON-RPC is the same
protocol shape MCP already uses, which means existing tooling
applies (logging, tracing, fault injection in tests).

### Alternative 4: Per-source secret store with no router

**Description:** Each source manages its own paths; the manifest
declares the source per secret directly; there is no global routing
layer.

**Why rejected:** This works for two or three secrets and breaks
once a team has a hundred. A team-wide migration from one Vault
mount to another would touch every manifest in every project.
Routing is exactly the indirection that lets such a migration be
one configuration change.

### Alternative 5: Always cache to disk

**Description:** Persist the cache between runs, encrypted with a
keychain-stored key.

**Why rejected:** The point of an external source is that it is the
authoritative copy. A persisted cache reintroduces stale-value
problems — exactly what ADR-005's keychain-only model already
exhibits in mixed environments. The fifteen-minute in-memory cache
is a UX accelerator, not a store.

## Implementation

- **Issues:**
  - [#246](https://github.com/meteora-pro/devboy-tools/issues/246) — design (ADR-020 + this ADR)
  - [#247](https://github.com/meteora-pro/devboy-tools/issues/247) — implementation, phased
- **Code (planned):**
  - `crates/devboy-storage/` — `SecretSource` trait, router,
    in-memory cache, source-credential resolution from
    `__sources/...`
  - `crates/plugins/secrets/keychain/` — built-in keychain source
    (refactor of existing `KeychainStore`)
  - `crates/plugins/secrets/1password/` — built-in 1Password CLI
    source
  - `crates/plugins/secrets/vault/` — built-in HashiCorp Vault
    source
  - `crates/plugins/secrets/env-store/` — built-in env-store source
    with `DEVBOY_SECRETS_FILE` loader
  - `crates/devboy-cli/` — `devboy secrets sources {list, install,
    refresh, validate}` subcommands and `doctor` integration
- **Subprocess protocol:** documented under
  `docs/guide/secrets/source-plugin-protocol.md` (planned).
- **Migration:** ADR-005 keychain remains the default for paths that
  do not match any route; existing entries continue to be readable
  after upgrade. ADR-020 migration tooling rewrites legacy keys to
  the new convention; this ADR's routing then picks them up.

## References

- [ADR-005: Credential storage](./ADR-005-credential-storage.md) —
  the prior single-backend layout this ADR generalises
- [ADR-019: Secrets carry SecretString end-to-end](./ADR-019-secret-string-discipline.md)
  — the type discipline that flows through the router unchanged
- [ADR-020: Secret manifest, path convention, and alias resolution](./ADR-020-secret-manifest-and-alias-resolution.md)
  — the namespace and manifest above the router
- [HashiCorp Vault — KV v2 secret engine](https://developer.hashicorp.com/vault/docs/secrets/kv/kv-v2)
- [1Password CLI — `op` reference](https://developer.1password.com/docs/cli/)
- [Model Context Protocol](https://modelcontextprotocol.io/) — the
  stdio JSON-RPC shape adopted by the subprocess plugin protocol

---

## Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-05-06 | Andrei Mazniak | Initial draft |
