---
id: ADR-021
title: External secret sources and backend routing
status: proposed
date: 2026-05-09
deciders: ["Andrei Mazniak"]
tags: ["security", "secrets", "plugins", "storage"]
supersedes: null
superseded_by: null
---

# ADR-021: External secret sources and backend routing

## Status

**proposed** (rewrite of the 2026-05-06 draft after design review)

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
- **Headless Linux machines without a Secret Service daemon** still
  need somewhere to store credentials at rest. Environment variables
  alone are not always acceptable (containers that shell out to
  child processes, dotfile managers that replicate `~`). An
  encrypted file backed by a passphrase is the natural fallback.
- **A single user routinely talks to several upstreams of the same
  type.** Two Vault servers under different addresses, two 1Password
  accounts (work and personal), two AWS profiles. Each carries its
  own credentials and possibly its own role.
- **The same upstream may need different credentials in different
  contexts.** An engineer may hold `read-only` and `deploy` tokens
  for the same Vault and want a context switch to flip which one is
  active, without re-authenticating.

ADR-020 left this surface deliberately empty. This ADR fills it.

### Differences from the 2026-05-06 draft

The previous draft of this ADR shipped four built-ins (`keychain`,
`1password`, `vault`, `env-store`) and a single in-memory cache TTL.
Design review surfaced four issues:

1. There was no fallback for headless machines without a keychain
   *and* without a configured external source. Falling all the way
   through to env-only was acceptable for CI but not for local
   off-network development.
2. The `Capabilities` enum did not distinguish backends that prompt
   for biometrics on every read (1Password CLI in the typical
   configuration) from backends that don't. The router could not
   warn the user when an agent loop would trigger N TouchID prompts.
3. The cache TTL was a flat default. Backends that return short
   leases (Vault dynamic secrets) became stale-by-design when the
   default outlived the lease.
4. CI behaviour was inferred from environment variables, not
   selected. A developer running a CI script locally, or a CI
   runner that did not set the expected variables, both got
   surprising routing.

The rewrite adds a fifth built-in (`local-vault`, with crypto and UX
detailed in [ADR-023](./ADR-023-secret-store-ux-layer.md)),
extends `Capabilities`, makes the cache TTL adaptive against upstream
lease duration, and turns CI mode into an explicit setting rather
than a heuristic.

## Decision

> **Decision:** The credential store is split into a thin **router**
> and a set of **secret-source plugins**. A secret path declared
> through the ADR-020 manifest resolves through the router to exactly
> one source, which in turn knows how to talk to its upstream
> (keychain, 1Password CLI, HashiCorp Vault, an env-store backend,
> the encrypted local vault from ADR-023, or a community-supplied
> subprocess plugin). The router is the only code that touches the
> manifest; the sources are interchangeable.

The decision has nine parts.

### 1. The `SecretSource` trait

A source is any backend able to answer questions about secrets. The
trait is small and explicitly capability-aware:

```rust
#[async_trait]
pub trait SecretSource: Send + Sync {
    fn name(&self) -> &str;
    fn capabilities(&self) -> Capabilities;     // see section 1.1
    fn requires_credential(&self) -> Option<CredentialRef>;  // see section 4

    async fn is_available(&self) -> SourceStatus;        // Available | Locked | NotInstalled | Error
    async fn get(&self, reference: &str) -> Result<Option<GetOutcome>>;
    async fn list(&self) -> Result<Vec<RemoteRef>>;       // optional, used by discovery
    async fn validate(&self, reference: &str) -> Result<()>;
}

pub struct GetOutcome {
    pub value: SecretString,
    pub lease_duration: Option<Duration>,  // see section 7
}
```

`reference` is a backend-specific string — for example
`op://Personal/GitHub PAT/credential` for 1Password,
`secret/data/team/gitlab#token` for Vault KV v2, a flat key for the
keychain, an environment-variable name for the env-store, an ADR-020
path itself for the local-vault. Sources do **not** know about ADR-020
paths beyond using them as references; mapping a path to a reference is
the router's job (section 2). This separation lets a source plugin be
written without any awareness of the manifest layer.

#### 1.1 Capabilities

```rust
bitflags! {
    pub struct Capabilities: u32 {
        const READ              = 0b0000_0001;
        const LIST              = 0b0000_0010;
        const VALIDATE          = 0b0000_0100;
        const WRITE             = 0b0000_1000;
        const ROTATE            = 0b0001_0000;
        const BIOMETRIC_PROMPT  = 0b0010_0000;
        const AUDIT_LOGGED      = 0b0100_0000;
    }
}
```

`READ`, `LIST`, `VALIDATE`, `WRITE`, `ROTATE` are operational. The
two new flags are descriptive — they let `doctor` and the agent
provisioning surface (see [ADR-023](./ADR-023-secret-store-ux-layer.md)
section 3.7) reason about UX trade-offs:

- `BIOMETRIC_PROMPT` — the source **may** require user-presence
  confirmation (biometric unlock or a PIN/passphrase prompt) on at
  least one of its operations in its default configuration. The
  flag is a single bit on the source as a whole — it does not
  encode "prompts only on writes" or "prompts only on reads", and
  the router does not infer per-operation cost from it. Sources
  whose reads are usually cached but whose writes always prompt
  (e.g. the local-vault, see §8) still set the flag. The router
  surfaces it as a `cost` hint to agents so that an agent loop
  doing twelve reads per minute does not blindly trigger twelve
  prompts; the agent should batch through a high-level provider
  tool instead.
- `AUDIT_LOGGED` — every read is durably logged on the upstream
  (Vault audit log, 1Password account activity). `doctor` shows
  this in the source-status section so the user knows their
  reads are observable.

A read-only source declares `READ | LIST | VALIDATE`; the 1Password
CLI in the typical biometric configuration declares
`READ | LIST | VALIDATE | BIOMETRIC_PROMPT | AUDIT_LOGGED`; an
env-store declares `READ` only; a Vault KV v2 source with sufficient
policy may declare `READ | LIST | VALIDATE | WRITE | ROTATE | AUDIT_LOGGED`.
Operations that require a missing operational capability fail with a
structured error rather than trying and erroring at the network
boundary.

### 2. Routing (`~/.devboy/secrets/sources.toml`)

Routing maps an ADR-020 path to a `(source, reference)` pair. The
configuration is global and lives at `~/.devboy/secrets/sources.toml`:

```toml
# Source definitions

[[source]]
name = "keychain"
type = "keychain"

[[source]]
name = "local-vault"
type = "local-vault"      # see ADR-023 for crypto and UX

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
fallback = "local-vault"           # see section 8

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
3. Otherwise, falling back to `[default].source`. If that source
   reports `is_available() == NotInstalled` and `[default].fallback`
   is set, the router retries against the fallback (typically the
   local-vault on a headless machine without keychain).

Path-to-reference mapping for prefix routes is source-specific and
defined by the source plugin (the keychain source uses the path
directly; the Vault source joins `mount` with the path tail; the
1Password source produces an `op://` URL from the `vault` setting and
the path tail; the local-vault source uses the path itself as the
reference).

### 3. Multiple instances of the same source type

Each `[[source]]` entry has a unique `name`. Two `vault` sources with
different `addr` values are two distinct sources, addressable
independently from routing. The `type` is what code is invoked; the
`name` is how users and routes refer to the source. This is the
mechanism by which a single user can talk to several upstreams of the
same flavour without confusion.

### 4. Per-context source credentials and the recursion invariant

A source may itself need credentials to operate (Vault token,
1Password account session, AWS profile). A source declares this in
the `requires_credential()` method on the trait. The default behaviour
is the source's native session management — `op signin`,
`vault login`, `aws configure sso`. ADR-005's keychain remains the
place for tokens that the system itself manages.

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

#### Recursion invariant

A source `A` that declares `requires_credential() = Some(...)` must
have its credential resolved through a source `B` whose
`requires_credential()` is `None`. The router enforces this at
configuration load: it walks the source-credentials graph, fails
with `E_SOURCE_CREDENTIAL_CYCLE` on any cycle, and fails with
`E_SOURCE_CREDENTIAL_DEEP` if the chain is longer than one hop.

The reasoning is simple: a Vault token cannot itself be stored in
Vault, because reading it would require Vault to already be
unlockable. The keychain (and the local-vault from ADR-023, once
unlocked) are the only sources that may hold source-credentials,
because they have no `requires_credential()` of their own.

In practice this means `__sources/<source-name>/<profile>` paths
**always** resolve to the keychain or the local-vault, regardless of
how the rest of the routing table is configured. The router enforces
this independently of the user-supplied `[[route]]` rules.

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

Built-in sources (keychain, local-vault, 1password, vault, env-store;
section 8) implement `SecretSource` directly inside the `devboy-tools`
binary. Community backends ship as separate executables, discovered at
`~/.devboy/plugins/secrets/devboy-source-<name>`, communicating with
the router over **JSON-RPC on stdio** — the same shape as MCP.

The protocol surfaces the trait directly:

```
secret_source.init        →  capabilities, version, requires_credential
secret_source.is_available →  status enum
secret_source.get         →  reference → { value, lease_duration? } | NotFound
secret_source.list        →  → RemoteRef[]
secret_source.validate    →  reference → ok | invalid | unreachable
```

#### Lifetime contract

The router enforces a strict lifecycle for plugin processes:

- **Spawn:** lazy, on first use. The router invokes the plugin with
  `Command::new(absolute_path)` and a restricted environment; only
  variables the plugin declared in its sidecar manifest
  (`devboy-source-<name>.toml`) are passed through.
- **Idle timeout:** the plugin is kept alive across calls for an
  idle window of **60 seconds** by default (configurable per
  source). After the window, the router sends `SIGTERM` and gives
  the plugin **10 seconds** to drain in-flight requests and exit
  cleanly.
- **Force kill:** if the plugin has not exited within the 10-second
  grace window, the router sends `SIGKILL`. Zombies are reported
  through `doctor` in the source-status section.
- **Crash recovery:** if the plugin exits with a non-zero status code
  while in use, the router restarts it transparently up to **three
  times within 60 seconds**. Beyond that, the source is marked
  `Error` and the router stops invoking it; a message in `doctor`
  tells the user to run `devboy secrets sources reset <name>` once
  the underlying issue is fixed.

The router never invokes source CLIs through a shell. Built-in sources
that wrap an external CLI (`op`, `vault`) follow the same rule:
absolute paths resolved at install/configure time, no shell-meta in
arguments. The plugin manifest may declare a checksum that `doctor`
verifies on each invocation; mismatch is a hard error.

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
expiring `SecretString`.

The TTL is **adaptive**:

- The **base TTL** is configurable per source (`cache_ttl_seconds`
  in the source definition) and defaults to **900 seconds**
  (15 minutes).
- If `get()` returns a `lease_duration`, the effective TTL is
  `min(base_ttl, lease_duration)`. This keeps the cache from
  outliving a Vault dynamic-secret lease. A `lease_duration` of
  `Some(0)` disables caching entirely for that read.
- A per-secret override may lower the TTL further through the
  global index (`cache_ttl_seconds_max` in the secret entry) but
  may not raise it above the source default.

Cache entries are dropped when:

- the effective TTL expires,
- the user invokes `devboy secrets refresh <path>` (or `--all`),
- a source declares an out-of-band invalidation (Vault lease
  revocation, 1Password session timeout — surfaced through
  `is_available()`),
- the process exits.

The cache is **never** persisted. Process exit drops every entry.
This is the same posture as `secrecy::SecretString::zeroize_on_drop`
extended one level up.

The local-vault daemon from ADR-023 holds its own cache of the
unlocked vault key, separate from this router cache. The router
sees a normal `get()` that returns a `SecretString` in microseconds;
the daemon's lifecycle (idle re-lock, PIN re-prompt for write
operations) is internal to the local-vault source.

### 8. Built-in sources

The first release ships five built-in implementations of
`SecretSource`. Together they cover the dominant deployment shapes
without requiring any community plugin.

#### `keychain`

A refactor of the existing `KeychainStore` from ADR-005, wrapped in
the `SecretSource` interface. `reference` is the path itself — the
keychain is the only source where the ADR-020 path doubles as the
backend reference. Capabilities: `READ | LIST | VALIDATE | WRITE`.
Available unless the platform exposes no keychain (headless Linux
without Secret Service); in that case `is_available()` returns
`NotInstalled` and the router falls back to `local-vault` if it is
configured as `[default].fallback`.

#### `local-vault`

An encrypted file at `~/.devboy/secrets/local-vault.dvb` unlocked
through a passphrase, a Touch-ID-wrapped key in the system keychain,
or a BIP39 recovery phrase. Crypto, daemon protocol, and UI flows are
the subject of [ADR-023](./ADR-023-secret-store-ux-layer.md).

`reference` is the ADR-020 path itself. Capabilities:
`READ | LIST | VALIDATE | WRITE | ROTATE | BIOMETRIC_PROMPT`. The
`BIOMETRIC_PROMPT` flag is always set because at least one operation
of the source can prompt for user presence — write/rotate always
require a fresh passphrase or biometric confirmation regardless of
the daemon's unlocked state, and reads can prompt when the unlocked
session has expired. Per the §1.1 contract, the bit reflects the
source as a whole and does not vary by operation.

The `local-vault` source is intended primarily for two scenarios:
headless Linux machines without a Secret Service daemon (where it is
the routed default), and any user who explicitly wants to keep some
paths out of the OS keychain. It is **not** intended as a third
parallel store next to keychain and external sources for routine use
— the router treats it as one option among many, and the routing
configuration is what determines when it is consulted.

#### `1password`

Backed by the `op` CLI. `reference` is an `op://` URL
(`op://<vault>/<item>/<field>`). Capabilities: `READ | LIST |
VALIDATE | BIOMETRIC_PROMPT | AUDIT_LOGGED`. Write capability is
omitted from the first release; `op item create` is muddled enough
that the ADR-020 `bootstrap` flow opens the 1Password UI for new
entries instead of writing through the CLI. `is_available()` checks
`op whoami`; a locked session returns `Locked` and `doctor` shows an
actionable message (`op signin`).

#### `vault`

Backed by the HashiCorp Vault HTTP API, KV v2. `reference` joins the
mount and path (`secret/data/<path>#<field>`, where `<field>` defaults
to `value`). Capabilities: `READ | LIST | VALIDATE | WRITE | ROTATE |
AUDIT_LOGGED`, subject to the policy associated with the active token.
Authentication methods are pluggable; the ADR ships token, AppRole,
and OIDC out of the box, with the active method configured per
source instance. `is_available()` calls `/sys/health` and a token
lookup; an expired token returns `Locked`.

The `vault` source's `get()` implementation populates
`lease_duration` from the upstream response; section 7's adaptive
TTL keeps the router cache honest against dynamic secrets.

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

#### CI mode (explicit, not heuristic)

CI behaviour is selected, not inferred. The router runs in **CI mode**
when **any** of the following holds:

- `DEVBOY_CI=1` (or `=true`) is set in the environment;
- the active context declares `[runtime] ci = true` in
  `~/.devboy/contexts/<name>.toml`;
- the CLI was invoked with `--ci`.

The router still **detects** CI heuristically (presence of `CI`,
`GITLAB_CI`, `GITHUB_ACTIONS`, `BUILDKITE`) and shows a `doctor`
notice "CI signals detected — but `DEVBOY_CI` is not set; routing
falls back to interactive defaults". This avoids two confusing
failure modes from the previous draft:

- A developer running a CI script locally with `CI=1` exported
  silently switching to env-store routing.
- A CI runner that does not set the expected variables silently
  staying on interactive routing and failing on the first
  keychain-unlock prompt.

In CI mode the router:

- promotes the `env-store` source to the front of the resolution
  chain regardless of the routing table;
- silently skips sources whose `is_available()` returns
  `NotInstalled`;
- refuses to invoke the local-vault unlock UI (no PIN prompt in CI);
- refuses to invoke biometric unlock (`BIOMETRIC_PROMPT` capability
  causes the source to be skipped);
- emits all decisions as structured logs at `info` level so a CI
  pipeline can grep for routing surprises.

### 9. `doctor` integration

`devboy doctor` gains two new sections:

- **Sources** — for each configured source, its name, type,
  availability status, capabilities (with `BIOMETRIC_PROMPT` /
  `AUDIT_LOGGED` flagged so the user knows the cost of each read),
  last successful contact, and any actionable message (`op signin`,
  `vault login`, `unset DEVBOY_SECRETS_FILE`, …).
- **Secrets in active context** — for each path declared in the
  active manifest, its routed source, current status (provisioned /
  expiring / missing / format-invalid), and (where the upstream
  exposes it) its expiry. Optional secrets are shown in their own
  sub-section without producing a non-zero exit. The CI-mode
  notice from section 8 surfaces here too.

Exit code is non-zero if any required secret is missing or
format-invalid. This makes `devboy doctor` a usable CI sanity gate
without further wrapping.

## Threat model adjustments

ADR-020 stated the framework's overall threat model. This ADR moves
two things on the spectrum:

- **Improvement.** External sources typically provide audit (every
  read is logged in 1Password and Vault — surfaced in
  `Capabilities` as `AUDIT_LOGGED`), centralized rotation, and
  per-team policy. A team that adopts an external source materially
  raises the bar over a per-developer keychain — not because
  `devboy-tools` is more secure, but because the upstream is.
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
- ✅ **CI works first-class.** The env-store is a real source, and
  CI mode is selected explicitly; routing is therefore predictable
  rather than implicit.
- ✅ **Headless Linux works without external infrastructure.** The
  local-vault fallback (ADR-023) makes "no keychain, no Vault, no
  1Password" a supported configuration rather than a workaround.
- ✅ **Multiple upstreams of the same type are addressable.** A user
  with several Vault servers or several 1Password accounts can
  configure them side by side under different `name`s.
- ✅ **Context-aware role switching.** A context switch flips
  which credential is presented to a source — a `read-only` token
  in one context, a `deploy` token in another.
- ✅ **A community plugin protocol exists from day one.** A new
  backend is a separate binary against a documented stdio protocol;
  it does not require forking `devboy-tools`.
- ✅ **Cached reads keep agents responsive.** The adaptive TTL
  honours upstream lease duration so dynamic-secret backends do
  not become stale-by-design, while still keeping biometric-prompt
  sources usable inside agentic loops.
- ✅ **Capability-aware UX.** `BIOMETRIC_PROMPT` and `AUDIT_LOGGED`
  let `doctor` and the agent surface (ADR-023 section 3.7)
  communicate the real cost and observability of each read.

### Negative

- ❌ **Two new configuration files.**
  `~/.devboy/secrets/sources.toml` and the local-vault file from
  ADR-023 join the ADR-020 `~/.devboy/secrets/index.toml` and the
  pre-existing `~/.devboy/config.toml`.
- ❌ **Built-in sources couple the binary to upstream CLI versions.**
  A breaking change in `op` or `vault` may surface as a new
  validation failure. **Mitigation:** integration tests in CI run
  against pinned versions of the upstream CLIs.
- ❌ **Subprocess plugins double the install surface for users who
  want non-built-in backends.** Discovery, signing, and update of
  plugins is itself a problem; this ADR ships the protocol but
  defers a managed plugin registry to a later decision.
- ❌ **Source-credential recursion is one rule the user has to
  internalise.** "Vault tokens cannot live in Vault" is obvious in
  retrospect but is one more concept on the onboarding path.
  **Mitigation:** the router enforces it; a confused configuration
  fails at load with `E_SOURCE_CREDENTIAL_CYCLE` and a pointer to
  the keychain or local-vault.

### Risks

- ⚠️ **Cache outliving an upstream rotation.** A secret rotated in
  Vault is still served from the in-process cache for up to the
  effective TTL (now bounded by `lease_duration`). **Mitigation:**
  the upper bound is bounded; sources expose `validate()` so a
  misbehaving downstream can be told to flush; `secrets refresh`
  is one command.
- ⚠️ **Misconfigured route silently routes to the wrong upstream.**
  A typo in a `prefix` lands a `team/...` path in the keychain
  instead of Vault. **Mitigation:** `devboy doctor` shows the
  resolved source for every required path; a route that resolves
  to the default source for a non-default prefix is flagged.
- ⚠️ **A compromised source CLI in PATH.** Covered in the threat
  model above; the mitigation is absolute paths plus checksums.
- ⚠️ **Subprocess plugin lifetime leaks.** A plugin that fails to
  exit cleanly leaves zombies. **Mitigation:** the lifetime
  contract in section 6 (60s idle, 10s grace, force kill, restart
  cap of 3 in 60s) is testable; acceptance tests in #247 Phase 5
  exercise crash recovery and zombie reporting.
- ⚠️ **CI heuristic divergence.** A user expecting CI mode because
  `CI=1` is set but not setting `DEVBOY_CI` may be surprised by
  interactive routing. **Mitigation:** `doctor` actively warns
  when CI signals are present without an explicit
  `DEVBOY_CI`/`--ci`.

## Alternatives Considered

### Alternative 1: Single static backend, configured globally

**Description:** Pick one backend per machine (keychain *or* Vault
*or* 1Password) and configure the whole tree to live there.

**Why rejected:** Real users hold mixes — a personal 1Password vault
plus a team Vault plus a CI env-store plus a headless box that runs
neither. A single-backend model forces either duplication (copy team
secrets into 1Password as well) or a manual ladder of fallbacks. The
routing layer is the price of honest support.

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
exhibits in mixed environments. The adaptive in-memory cache is a
UX accelerator, not a store. The local-vault from ADR-023 is the
right answer for "I want a persistent encrypted store" — it is
exposed as a source, not a cache.

### Alternative 6: Heuristic CI mode (the prior draft)

**Description:** Auto-detect CI from `CI` / `GITLAB_CI` /
`GITHUB_ACTIONS` and silently switch routing.

**Why rejected:** Both directions of the heuristic surprise the
user — a developer with `CI=1` set locally falls into env-store
routing, and a CI runner that does not set the expected variables
falls out of it. CI mode is now an explicit setting; the heuristic
becomes a `doctor` notice.

### Alternative 7: Flat 15-minute cache TTL (the prior draft)

**Description:** A single default TTL for all sources, configurable
per source but flat per get().

**Why rejected:** Vault dynamic secrets routinely have leases
shorter than 15 minutes; the flat default served stale values until
the user manually refreshed. The adaptive TTL
(`min(default, lease_duration)`) costs almost nothing to implement
and removes a whole class of confusing bugs.

## Implementation

- **Issues:**
  - [#246](https://github.com/meteora-pro/devboy-tools/issues/246) — original design (ADR-020 + this ADR)
  - [#247](https://github.com/meteora-pro/devboy-tools/issues/247) — implementation, phased
  - To be filed — design refresh covering the rewritten ADR-020/021 and the new ADR-023
- **Code (planned):**
  - `crates/devboy-storage/` — `SecretSource` trait, router,
    in-memory cache (with adaptive TTL), source-credential
    resolution from `__sources/...`, recursion check
  - `crates/plugins/secrets/keychain/` — built-in keychain source
    (refactor of existing `KeychainStore`)
  - `crates/plugins/secrets/local-vault/` — built-in encrypted
    vault source (crypto and daemon detailed in ADR-023)
  - `crates/plugins/secrets/1password/` — built-in 1Password CLI
    source
  - `crates/plugins/secrets/vault/` — built-in HashiCorp Vault
    source
  - `crates/plugins/secrets/env-store/` — built-in env-store source
    with `DEVBOY_SECRETS_FILE` loader
  - `crates/devboy-cli/` — `devboy secrets sources {list, install,
    refresh, validate, reset}` subcommands and `doctor` integration
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
- [ADR-023: Secret store UX layer](./ADR-023-secret-store-ux-layer.md)
  — encrypted local vault crypto, daemon protocol, native UI,
  manual-assisted rotation, pattern catalogue, agent provisioning
  protocol, `setup-secrets` skill
- [HashiCorp Vault — KV v2 secret engine](https://developer.hashicorp.com/vault/docs/secrets/kv/kv-v2)
- [1Password CLI — `op` reference](https://developer.1password.com/docs/cli/)
- [Model Context Protocol](https://modelcontextprotocol.io/) — the
  stdio JSON-RPC shape adopted by the subprocess plugin protocol

---

## Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-05-06 | Andrei Mazniak | Initial draft |
| 2026-05-09 | Andrei Mazniak | Rewrite after design review: 5th built-in `local-vault` (UX in ADR-023); `BIOMETRIC_PROMPT` / `AUDIT_LOGGED` capabilities; adaptive cache TTL bounded by upstream `lease_duration`; explicit source-credential recursion invariant; CI mode as an explicit setting (no longer a heuristic); subprocess plugin lifetime contract spelled out (60s idle / 10s grace / restart cap) |
