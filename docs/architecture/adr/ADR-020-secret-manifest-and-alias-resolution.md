---
id: ADR-020
title: Secret manifest, path convention, and alias resolution
status: proposed
date: 2026-05-09
deciders: ["Andrei Mazniak"]
tags: ["security", "secrets", "manifest", "core"]
supersedes: null
superseded_by: null
---

# ADR-020: Secret manifest, path convention, and alias resolution

## Status

**proposed** (rewrite of the 2026-05-06 draft after design review)

## Context

[ADR-005](./ADR-005-credential-storage.md) decided **where** a secret lives:
the OS keychain, with an environment-variable fallback for CI and headless
hosts. [ADR-019](./ADR-019-secret-string-discipline.md) decided **how** a
secret is typed in transit through the process: `secrecy::SecretString`
end-to-end, redacted on `Debug`, zeroized on `Drop`.

Neither of those addresses how a secret is **declared**, **discovered**,
**referenced**, or **validated**. In practice this leaves several real
problems on the table:

- **No discoverability.** A token sits in the keychain under a key like
  `gitlab.token`. Nobody but the person who entered it knows what it is for,
  how to obtain a fresh one, what format it should have, when it expires, or
  which providers in the codebase actually consume it.
- **No project-level "map of required secrets".** A new contributor cloning
  a repo cannot answer "which credentials do I need to set up to make this
  project work?" without reading source code or asking a teammate.
- **Plaintext still leaks through human-driven routes.** Even with
  `SecretString` inside the process, the secret value is still typed into
  shell prompts, pasted into agent chat windows, exported into shell
  rc-files, and committed to `.env` files. Any of those flows ends up in
  shell history, terminal scrollback, agent transcripts, or git history.
- **No validation.** A typo in a token is detected only on the first real
  HTTP call that uses it; the failure mode is a generic 401 from a
  third-party API, not "the value you stored doesn't match the expected
  format for this provider".
- **No expiry tracking and no rotation cadence.** Personal access tokens
  commonly expire after 90 days. Nothing in the current system warns the
  user before that happens or records when the token was last rotated.
- **No boundary between contexts.** If an agent operates in `context A`
  but asks for a credential that "logically" belongs to `context B`, the
  request succeeds silently as long as the keychain entry exists. There
  is no manifest of "what `context A` is allowed to ask for".
- **No way for an agent to ask for a missing secret without seeing
  values.** A coding agent working on a repo can detect "this provider
  isn't configured" only by failing. It cannot proactively ask the user
  to provision a named secret without the agent itself becoming a
  potential leak surface.

This ADR introduces a layer **above** the credential store: a manifest of
known secrets with metadata, a strict naming convention, an alias-based
referencing scheme, and a validation framework. The credential store
defined by ADR-005 stays where it is — this ADR adds discovery,
declaration, and resolution discipline on top of it.

External secret sources (1Password CLI, HashiCorp Vault, AWS Secrets
Manager, an env-only store for CI, an encrypted local vault, etc.) are
out of scope here and are the subject of [ADR-021](./ADR-021-external-secret-sources.md).
The user-facing UX layer (encrypted local vault crypto, native UI,
manual-assisted rotation flow, agent provisioning protocol, onboarding
skill) is the subject of [ADR-023](./ADR-023-secret-store-ux-layer.md).

### Differences from the 2026-05-06 draft

The previous draft of this ADR insisted that a secret's metadata
(`description`, `retrieval_url`, …) lives **only** in the global index
and that the per-project manifest carries only references and behavioural
overrides. Design review surfaced two reasons that rule was too strict:

1. Some secrets are genuinely project-local (a sandbox account's API key,
   a one-off integration token used by exactly one repo). Forcing them
   into the global index creates a registry of secrets that no one else
   needs to know about.
2. Project-specific *context* — "this token is the one our CI uses for
   the staging deploy" — is useful next to the manifest entry, even when
   a global description already exists.

The rewrite therefore allows per-project metadata in addition to global,
with explicit conflict-resolution rules and a `doctor` warning when the
two diverge. See section 4 below.

### Threat model (explicit)

This ADR protects against **accidental** secret leakage by humans, by
agents acting in good faith, and by routine tooling:

- Plaintext values in agent transcripts, configuration files, shell
  history, terminal scrollback, `ps` output, crash dumps, log files, or
  committed `.env` files.
- A teammate accidentally pasting a value from one context into a
  configuration that belongs to another.
- Drift between "what the project needs" and "what is actually
  provisioned on this machine".

This ADR explicitly does **not** claim isolation against a malicious or
compromised agent. An agent that can run a shell can always
`echo $SOMETHING`, dump `/proc/self/environ`, or read any file the user
can read. Full isolation requires running the agent in a sandboxed
process and is out of scope.

## Decision

> **Decision:** A secret in `devboy-tools` has a fixed name (its **path**)
> drawn from a single global namespace, declared in a project-level
> **manifest** that is committed to source. Metadata may live in either
> the **global index** or the per-project manifest, with the per-project
> manifest taking precedence in conflicts. Code, configuration, and
> command lines reference a secret by its path through the alias form
> `@secret:<path>`; values are resolved on demand through the existing
> credential store and never stored alongside the reference.

The decision has eight parts.

### 1. A single global flat namespace

All secrets known to `devboy-tools` share one flat key space. There is no
per-context namespace, no per-project namespace, no nesting of stores.
Two contexts that need different values for "the same kind of credential"
**must** declare different paths — for example
`team/gitlab/token-deploy` and `client-acme/gitlab/token-deploy`.

The motivation is reuse: a personal token (`personal/github/pat`) is
typed once and referenced from every project that needs it, without the
user having to maintain N copies in N keychain entries.

The trade-off — that any process running as the user can read any path
in the namespace — is the same boundary that already exists in the OS
keychain (ADR-005). This ADR does not weaken it; the soft enforcement
described in section 7 strengthens it for agent-mediated access.

### 2. Hard path convention

A path is a `/`-separated sequence of **segments**. The validator rejects
a path that does not match all of the following:

- Minimum **three** segments. The shape is `<scope>/<provider>/<purpose>`.
  Two-segment paths (`gitlab/token`) are intentionally not allowed —
  they have no scope and silently encourage cross-context reuse of
  credentials that should be distinct.
- Each segment matches `[a-z][a-z0-9-]*`. Lowercase, kebab-case, no
  dots, no underscores, no slashes inside a segment.
- The first segment is the **scope**. It is open-ended (not a fixed
  enum), but conventional values are `team`, `personal`,
  `client-<short-name>`, and `sandbox`. Tooling lints unknown scopes
  but does not reject them.
- Two prefixes are **reserved** by the framework and rejected from
  user-facing paths:
  - `__*` — internal use (for example, source authentication
    credentials, see ADR-021). Hidden by default in `secrets list`.
  - `_test/*` — paths used by the test suite. Production code refuses
    to read them.

Examples of valid paths:

```
team/gitlab/token-deploy
team/openai/api-key
personal/github/pat
personal/anthropic/api-key
client-acme/jira/api-key
sandbox/example-provider/token
```

Examples that are rejected:

```
gitlab.token              # too few segments, dot separator
GitLab/Token              # not lowercase, not kebab-case
team/gitlab               # too few segments
team//gitlab/token        # empty segment
__sources/vault-a/token   # reserved prefix in user-facing context
```

The convention is enforced as a **hard error** — at manifest load time,
at resolver lookup time, and in the CLI. The reasoning is that an
inconsistent namespace silently degrades into "every project invents
its own pattern", which is the situation this ADR is designed to fix.

### 3. Global index (`~/.devboy/secrets/index.toml`)

The global index holds **metadata, never values**. It is a TOML file
under `~/.devboy/secrets/index.toml`. Each entry is keyed by path and
carries:

```toml
[secret."team/gitlab/token-deploy"]
description       = "Deploy token for the team GitLab; used by CI mirrors and devboy plugins"
retrieval_url    = "https://gitlab.example.internal/-/profile/personal_access_tokens"
format_regex      = "^glpat-[A-Za-z0-9_-]{20,}$"
default_gate      = "auto"        # auto | confirm | touchid
expires_at        = "2026-08-01"  # optional, populated by validation if upstream exposes it
last_rotated_at   = "2026-05-02"  # optional, advisory
rotate_every_days = 90            # optional, drives doctor warnings
rotation_method   = "manual"      # manual | provider-ui | provider-api (reserved for future)
required_scopes   = ["api", "read_repository"]  # optional, advisory
pattern_id        = "gitlab-pat"  # optional, reference into devboy-secret-patterns
```

`pattern_id` is a new field that links the entry to a built-in pattern
in the `devboy-secret-patterns` crate (see [ADR-023](./ADR-023-secret-store-ux-layer.md)
section 3.6). When set, the pattern supplies sensible defaults for
`format_regex`, `retrieval_url`, `rotation_method`, and `default_expiry_days`
that the entry inherits unless explicitly overridden.

No secret value is ever written to the index. The credential store from
ADR-005 (and the source router from ADR-021) remains the only place
values live.

### 4. Per-project manifest (`.devboy/secrets.toml`)

A project that uses `devboy-tools` declares its dependency on secrets
in a manifest committed to the repository:

```toml
# .devboy/secrets.toml

required = [
    "team/gitlab/token-deploy",
    "personal/github/pat",
    "personal/anthropic/api-key",
    "sandbox/example-provider/token",
]

optional = [
    "personal/slack/notify-token",   # the notify skill works without it
]

# Per-secret overrides — behavioural fields and project-local description.
# Other metadata (retrieval_url, format_regex, rotation_method) is read
# from the global index only.

[overrides."team/gitlab/token-deploy"]
gate              = "touchid"   # this project tightens the default gate
rotate_every_days = 30          # this project requires more frequent rotation
description       = "Used by the staging deploy pipeline; contact ops before rotating"

# A path may be declared project-local — its metadata lives in this manifest
# and is not registered in the global index. This is intended for genuinely
# single-project secrets (sandbox accounts, one-off integration tokens).

[secret."sandbox/example-provider/token"]
description    = "Token for the example-provider sandbox account used only by this repo"
retrieval_url = "https://example-provider.dev/account/api-tokens"
pattern_id     = "generic-bearer"
```

The manifest carries three things:

- A list of **required** and **optional** paths. `doctor` fails the
  exit code on a missing required path; missing optional paths surface
  as informational.
- Behavioural **overrides** — `gate`, `rotate_every_days`, and a
  project-local `description` — for paths whose canonical metadata
  lives in the global index. Overrides for any other field are
  rejected by the loader.
- **Project-local metadata** for paths declared as project-owned (the
  `[secret."..."]` block). The loader treats such a path as if its
  global-index entry were absent: the manifest is the source of truth.

#### Conflict resolution rules

The loader merges metadata for each required/optional path according to
the following precedence:

1. If the path appears as `[secret."<path>"]` in the manifest, its
   metadata is read from the manifest only. The path is project-local.
2. Otherwise, metadata is read from the global index. If the manifest
   has an `[overrides."<path>"]` block, the listed behavioural fields
   override the index values.
3. If a path appears in `required` / `optional` and exists in **neither**
   the global index nor as `[secret."..."]`, the loader fails with
   `E_SECRET_UNKNOWN_PATH` and points at `devboy secrets bootstrap` or
   `devboy secrets describe <path>` to register it.

If `[overrides."<path>"]` mentions a field that is not one of the allowed
behavioural fields (for example, attempts to override `format_regex`),
the loader fails with `E_OVERRIDE_FIELD_NOT_ALLOWED`. This is enforced as
a hard error rather than a warning so that drift between the index and
the manifest cannot grow silently.

`devboy doctor` additionally surfaces a **warning** (not an error) when
an `[overrides]` value is the same as the corresponding global value —
the override is a no-op and should be removed.

Committing the manifest gives a team three things:

- **Onboarding-as-data.** A new contributor runs `devboy secrets
  bootstrap` (or invokes the `setup-secrets` skill — see ADR-023) and
  walks through every required secret with system prompts; missing
  entries are filled in interactively.
- **Visibility of cross-project reuse.** Tooling can compute "this
  secret is referenced by N projects" by walking known manifests.
- **A target for review.** A pull request that adds a new required
  secret is now an explicit, reviewable change to the manifest, not a
  hidden side effect of "code that started reading a new env var".

A project may keep both `required` and `optional` empty; the manifest
then asserts "this project does not currently depend on any managed
secret". The validator treats an absent manifest as equivalent to an
empty one — opt-in, no cost for projects that do not need it.

### 5. Alias resolution (`@secret:<path>`)

Configuration files, command lines, and HTTP request templates may
reference a secret by its path through the alias form `@secret:<path>`.
The resolver expands the alias on demand at the smallest possible
scope. The resolver hands the expanded value back as
`secrecy::SecretString` (per ADR-019); plaintext is exposed only through
`.expose_secret()` at the call site.

Four substitution points are supported:

- **Config files.** A config loader that encounters `@secret:<path>`
  in a string field resolves it through the credential chain. Example:

  ```toml
  [gitlab]
  token = "@secret:team/gitlab/token-deploy"
  ```

  The TOML on disk holds the alias, never the value.

- **External command argv.** A wrapper rewrites `@secret:<path>`
  occurrences in argv before `exec`. Because argv is visible to other
  processes through `ps`, the wrapper prefers passing the secret
  through stdin or a file descriptor when the target tool supports it
  (for example, `gh auth login --with-token`, `git credential fill`).
  Direct argv substitution is the fallback for tools that accept
  secrets only in argv, and is documented as such.

- **HTTP through the local proxy.** The MCP proxy already mediates
  outgoing HTTP for some flows. When it sees an outgoing
  `Authorization: Bearer @secret:<path>` (or another whitelisted
  header pattern), it rewrites the value to the resolved secret
  before forwarding. The agent that constructed the request never
  saw the value; the request as logged through transcript shows the
  alias.

- **MCP tool requests from agents.** An agent that needs a value to
  perform a high-level operation calls a typed MCP tool. Per
  [ADR-023](./ADR-023-secret-store-ux-layer.md) section 3.7, agents
  do **not** have a `secrets.get` tool; the only legitimate path for
  a value to be used is through a high-level provider tool
  (`gitlab.create_merge_request`) where `devboy-tools` resolves the
  credential server-side and the value never crosses the agent
  boundary.

The alias prefix is `@secret:` rather than something like `${SECRET:...}`
so that it cannot be accidentally interpreted by a shell expansion or
by a templating engine that doesn't know about `devboy-tools`.

A pre-commit hook (`devboy hooks install --secret-alias-lint`) is
provided as a defensive measure: if an unresolved `@secret:<path>` ends
up committed in a file that is **not** known to be alias-aware (for
example, an arbitrary `.env` file that is not loaded through the
`devboy-tools` config loader), the hook fails the commit and prints
the offending file and line.

### 6. Validation framework

Each entry in the global index — or the equivalent project-local entry
— can declare validation. There are three levels, executed lazily and
on demand by `devboy secrets validate`, `devboy doctor`, and (optionally)
on `bootstrap`:

- **Format validation.** A `format_regex` in the index, or one inherited
  from a `pattern_id`. Cheap, runs offline, catches typos and
  accidental `gh_xxx` vs `ghp_xxx` mixups.
- **Liveness validation.** Provider plugins (the existing API plugins
  under `crates/plugins/api/*`) expose a `test` method that performs
  a known-cheap authenticated call. Liveness is opt-in per secret;
  the validator looks up the provider from the path's second segment
  unless an explicit `validation` block names a different one. The
  `devboy-secret-patterns` crate (ADR-023) supplies a default liveness
  recipe per pattern (e.g., `GET /user` for GitHub).
- **Expiry and rotation tracking.** When the upstream API returns an
  expiry (GitLab and GitHub PATs do, for example), liveness validation
  records `expires_at` back into the global index. `rotate_every_days`
  paired with `last_rotated_at` produces advisory warnings. `doctor`
  surfaces "expires in N days" warnings under a fixed threshold
  (default seven days) and may invoke the `notify` skill if the user
  has wired it up.

Validation never reads values from anywhere other than the credential
store, never logs them, and never writes them to the index.

### 7. Soft enforcement through manifest gating

The MCP API exposes only **non-value** secret tools to agents — see
[ADR-023](./ADR-023-secret-store-ux-layer.md) section 3.7 for the full
list. The router-side resolver, used by high-level provider tools, also
enforces a manifest gate: a path that exists in the global keychain but
is not declared in the active context's manifest yields a structured
error (`E_SECRET_NOT_IN_MANIFEST`) rather than the secret value.

This is **soft enforcement**, not isolation:

- An agent that can spawn shells can still read any keychain entry by
  invoking the OS-level CLI directly. The gate raises the visible
  cost: a sanctioned access goes through the typed MCP API and leaves
  a structured record; an unsanctioned access has to invoke a shell
  and is therefore visible in the transcript.
- A `--allow-cross-context` flag on the MCP request opts out of the
  gate for one call, with an audit log entry. This is intended for
  one-off operations (cross-project tooling, migrations).

The benefit is that the dominant accidental-misuse pattern — an agent
"helpfully" reaching for whichever credential happens to be in the
keychain — fails closed and produces an error message that names the
manifest as the place to add the dependency.

### 8. Migration from ADR-005 keychain entries

Existing keychain entries written under the legacy
`<provider>.<credential>` shape (per ADR-005) are not valid paths
under section 2. Migration is a two-step process:

1. **`devboy doctor` reports nonconformant entries** as a new check
   `secrets-path-convention`. Each row names the legacy key, the
   suggested canonical path, and the `devboy secrets migrate <legacy>`
   one-liner.
2. **`devboy secrets migrate`** is a CLI subcommand that walks an
   interactive flow per entry: confirm or edit the suggested path,
   choose `scope` (`team` / `personal` / `client-...` / `sandbox`),
   write the new keychain entry through the source router, register
   the path in the global index, and optionally delete the legacy
   entry. The original entry is left alone unless the user explicitly
   asks for cleanup.

Until the migration is complete, both shapes coexist:

- The legacy `CredentialStore::get` API continues to read legacy keys
  for code that has not yet been moved over to alias resolution.
- The new manifest layer ignores legacy keys; references through
  `@secret:<path>` use only the new namespace.

A flag in `~/.devboy/config.toml`
(`[secrets] migration_complete = true`) marks the keychain as fully
migrated and disables the legacy reader, so a stale legacy entry cannot
be silently picked up after migration.

## Consequences

### Positive

- ✅ **Onboarding becomes data, not lore.** A new contributor runs
  `setup-secrets` (the skill from ADR-023, which delegates to
  `devboy secrets bootstrap`), the manifest drives an interactive walk
  through every required secret, and the system prompts use the
  retrieval hint to point at the right page.
- ✅ **Reuse without duplication.** A personal token is provisioned
  once and referenced from every project; rotation happens in one place.
- ✅ **Project-local secrets are first-class.** A genuine one-off
  token does not pollute the shared global index.
- ✅ **Aliases in code, not values.** `@secret:<path>` makes plaintext
  in committed configuration a contradiction: a value next to the
  alias means someone bypassed the resolver. The pre-commit hook
  catches the obvious miss-pastes.
- ✅ **Drift becomes visible.** A secret that expires, fails liveness,
  or is missing surfaces in `doctor`; a secret that is referenced
  with a non-conformant path fails validation at load time; an
  override that no longer differs from the global value warns.
- ✅ **Manifest review.** A change to required secrets is a change
  to a tracked file, reviewable in a pull request.

### Negative

- ❌ **Two new files in the user's home directory.**
  `~/.devboy/secrets/index.toml` (metadata) is added on top of the
  existing `~/.devboy/config.toml`. ADR-021 adds a third
  (`sources.toml`); ADR-023 adds a fourth (`local-vault.dvb`).
- ❌ **One new file in each project that opts in.** `.devboy/secrets.toml`
  is small but it is one more committed file.
- ❌ **Hard path convention is a one-way migration.** Existing
  keychain entries with names like `gitlab.token` are not valid paths
  under the new convention; the migration tool covers the mechanics
  but the user has to walk through it once per machine.
- ❌ **Two valid sources of metadata.** Per-project and global both
  carry metadata, and the precedence rules have to be learned. The
  loader's hard error on disallowed override fields and `doctor`'s
  no-op-override warning keep the surprise small, but it is more to
  document than a single source.

### Risks

- ⚠️ **Manifest commits leak intent.** A committed manifest reveals
  which providers a project uses, even if no values leak. For a
  public OSS repository this is usually intended (it's part of the
  README); for a private repository describing a sensitive
  integration, the project may prefer to keep the manifest in a
  private companion repo. **Mitigation:** the manifest path is
  configurable through `~/.devboy/config.toml`; teams can place it
  outside the main repo if needed.
- ⚠️ **Alias-bypass via direct env-var read.** A library or tool
  that reads `process.env.GITLAB_TOKEN` directly (without going
  through the resolver) bypasses the alias system entirely.
  **Mitigation:** scope of this ADR is `devboy-tools`-mediated
  configuration; third-party tools using their own conventions are
  out of scope. The CI grep gate from ADR-019
  (`secrets-discipline`) flags direct token reads inside `crates/`.
- ⚠️ **`@secret:` aliases in unmediated config.** A user copies an
  alias-bearing config snippet into a tool that does not understand
  `@secret:`, and the tool sends the literal string `@secret:...` as
  the credential. **Mitigation:** the alias prefix is documented and
  conspicuous; the wrapper tools explicitly fail closed when
  encountering an unresolved alias at exec time; the pre-commit hook
  flags unresolved aliases in committed files.
- ⚠️ **Validation false negatives mask real expiry.** If a provider
  API returns 200 even with a token that is two days from expiring,
  liveness validation does not catch it. **Mitigation:** rotation
  reminders driven by `last_rotated_at` and `rotate_every_days` are
  independent of liveness and run regardless of upstream behaviour.
- ⚠️ **Per-project metadata divergence.** A project-local
  `description` for a globally-known path can drift from the global
  one over time. **Mitigation:** the manifest's `[overrides]` block
  is the only place to add a project-local description for a global
  path; the loader's no-op-override warning is the existing
  cleanup signal.

## Alternatives Considered

### Alternative 1: Per-context flat namespace

**Description:** Make every secret implicitly scoped by context, so
`gitlab/token` in `context A` and `gitlab/token` in `context B` are two
distinct keys with two distinct values.

**Why rejected:** This produces N copies of personal credentials that
are genuinely the same value, multiplies rotation work by N, and hides
which contexts actually share a credential. The user explicitly
preferred a single flat namespace with the discipline of "different
values must have different paths" enforced as a convention.

### Alternative 2: Vault-style hierarchical store with policy engine

**Description:** Model the namespace as a tree (like HashiCorp Vault's
KV engine) with a per-context policy that grants or denies access to
sub-trees.

**Why rejected:** A policy engine is large surface area for a local
CLI tool and substantially raises the floor of "what you must
configure before you can use the system". The soft-enforcement
mechanism in section 7 captures the most useful 10% of the policy
behaviour (manifest-as-policy) at near-zero configuration cost. If a
real policy engine is needed in the future, it can be introduced as a
separate decision, ideally by delegating to an external source (see
ADR-021).

### Alternative 3: Global index is the only source of metadata (the prior draft)

**Description:** Do not allow per-project metadata. Any secret
referenced from a manifest must first be described in the global
index. The manifest carries only references and behavioural overrides.

**Why rejected:** Genuinely project-local secrets (sandbox accounts,
one-off integration tokens) are forced into a registry that no other
project needs. Project-specific *context* — "in this repo this token
is the staging deploy one" — has nowhere to live except as duplication
in code comments. The current decision keeps the global index as the
canonical source for shared metadata, but allows per-project
description and project-local entries for the long tail.

### Alternative 4: Encrypted file with embedded values (sealed manifest)

**Description:** Combine values and metadata into one file encrypted
with a per-user master key (age, sops, gpg).

**Why rejected:** A sealed file requires a master-key prompt on every
invocation (or a long-running agent process holding the key in
memory). [ADR-023](./ADR-023-secret-store-ux-layer.md) introduces
exactly such an agent-and-vault pair as a *fallback* source for
environments without a keychain — but that is one source among several
managed by the router (ADR-021), not the single transport for all
metadata. Forcing every metadata read through a vault-unlock prompt
would be the regression ADR-005 already considered and rejected.

### Alternative 5: Free-form path convention with linting only

**Description:** Allow any non-empty string as a path; lint deviations
from the recommended shape rather than rejecting them.

**Why rejected:** The current state (free-form `<provider>.<credential>`
keys per ADR-005) is exactly the lint-only baseline. Drift between
projects has already happened in practice. The hard rule is the
cheapest mechanism for preventing the namespace from re-fragmenting.

## Implementation

- **Issues:**
  - [#246](https://github.com/meteora-pro/devboy-tools/issues/246) — original design (this ADR + ADR-021)
  - [#247](https://github.com/meteora-pro/devboy-tools/issues/247) — implementation, phased
  - To be filed — design refresh covering the rewritten ADR-020/021 and the new ADR-023
- **Code (planned):**
  - `crates/devboy-storage/` — extend with manifest loader, global
    index, path validator, override merge logic
  - `crates/devboy-core/` — config-loader integration with
    `@secret:` resolution
  - `crates/devboy-executor/` — argv-substitution wrapper with
    stdin/FD pass-through
  - `crates/devboy-mcp/` — manifest gating, `Authorization` header
    rewriting in `proxy.rs`
  - `crates/devboy-cli/` — `devboy secrets {list, describe, validate,
    bootstrap, migrate}` subcommands; `devboy hooks install
    --secret-alias-lint`
- **Migration:** legacy keychain entries from ADR-005 remain readable
  through the existing API until `[secrets] migration_complete = true`
  is set in `~/.devboy/config.toml`. The migration tool walks them,
  asks the user for the canonical path under the new convention, and
  rewrites the index. Tracked as a separate phase in #247.

External secret sources, source routing, and per-context source
credentials are deferred to ADR-021. The encrypted local vault, the
native UI, the manual-assisted rotation flow, the pattern catalogue,
the agent provisioning protocol, and the `setup-secrets` skill are
deferred to ADR-023.

## References

- [ADR-005: Credential storage](./ADR-005-credential-storage.md) — where
  values live (keychain + env fallback)
- [ADR-019: Secrets carry SecretString end-to-end](./ADR-019-secret-string-discipline.md)
  — how values are typed in transit through the process
- [ADR-021: External secret sources](./ADR-021-external-secret-sources.md)
  — pluggable backends sitting behind the same path namespace
- [ADR-023: Secret store UX layer](./ADR-023-secret-store-ux-layer.md)
  — encrypted local vault, native UI, agent provisioning protocol,
  pattern catalogue, manual-assisted rotation, `setup-secrets` skill
- [`secrecy` crate documentation](https://docs.rs/secrecy/)
- [HashiCorp Vault — KV namespace concepts](https://developer.hashicorp.com/vault/docs/secrets/kv) — inspiration for path-based addressing

---

## Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-05-06 | Andrei Mazniak | Initial draft |
| 2026-05-09 | Andrei Mazniak | Rewrite after design review: per-project metadata is allowed alongside global; conflict-resolution rules added; migration tooling section made explicit; references to ADR-023 (UX layer) added |
