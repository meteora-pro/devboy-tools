---
id: ADR-020
title: Secret manifest, path convention, and alias resolution
status: proposed
date: 2026-05-06
deciders: ["Andrei Mazniak"]
tags: ["security", "secrets", "manifest", "core"]
supersedes: null
superseded_by: null
---

# ADR-020: Secret manifest, path convention, and alias resolution

## Status

**proposed**

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
- **No expiry tracking.** Personal access tokens commonly expire after
  90 days. Nothing in the current system warns the user before that
  happens.
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
Manager, an env-only store for CI, etc.) are out of scope here and are
the subject of [ADR-021](./ADR-021-external-secret-sources.md).

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
> **manifest** that is committed to source, with metadata kept in a global
> **index**. Code, configuration, and command lines reference a secret by
> its path through the alias form `@secret:<path>`; values are resolved
> on demand through the existing credential store and never stored
> alongside the reference.

The decision has six parts.

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
retrieval_hint    = "https://gitlab.example.internal/-/profile/personal_access_tokens"
format_regex      = "^glpat-[A-Za-z0-9_-]{20,}$"
default_gate      = "auto"        # auto | confirm | touchid
expires_at        = "2026-08-01"  # optional, populated by validation if upstream exposes it
last_rotated_at   = "2026-05-02"  # optional, advisory
rotate_every_days = 90            # optional, drives doctor warnings
required_scopes   = ["api", "read_repository"]  # optional, advisory
```

No secret value is ever written to the index. The credential store from
ADR-005 remains the only place values live.

The index is the **source of truth for ownership of metadata**. A
per-project manifest (next section) may not invent metadata for a path
that is not described in the global index. The reasoning: if every
project can invent its own description and `retrieval_hint`, the
metadata diverges, defeating the purpose of a shared namespace. New
paths must first be described in the global index (interactively
during `secrets bootstrap`, or by hand), and only then be referenced
from projects.

### 4. Per-project manifest (`.devboy/secrets.toml`)

A project that uses `devboy-tools` declares its dependency on secrets
in a manifest committed to the repository:

```toml
# .devboy/secrets.toml
required = [
    "team/gitlab/token-deploy",
    "personal/github/pat",
    "personal/anthropic/api-key",
]

optional = [
    "personal/slack/notify-token",   # the notify skill works without it
]

[overrides."team/gitlab/token-deploy"]
gate = "touchid"   # this project tightens the default gate for this secret
```

The manifest contains **only references and project-local overrides**.
No values, no descriptions, no retrieval hints — those live in the
global index and apply uniformly across every project that references
the same path.

Committing the manifest gives a team three things:

- **Onboarding-as-data.** A new contributor runs `devboy secrets
  bootstrap` and walks through every required secret with system
  prompts; missing entries are filled in interactively.
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
  perform a high-level operation calls a typed MCP tool such as
  `secrets.get(path)`, subject to the soft enforcement described in
  section 7. The preferred mode, however, is for the agent to call
  the high-level provider tool directly (`gitlab.create_merge_request`)
  and let `devboy-tools` resolve the credential server-side, so the
  value never crosses the agent boundary at all.

The alias prefix is `@secret:` rather than something like `${SECRET:...}`
so that it cannot be accidentally interpreted by a shell expansion or
by a templating engine that doesn't know about `devboy-tools`.

### 6. Validation framework

Each entry in the global index can declare validation. There are three
levels, executed lazily and on demand by `devboy secrets validate`,
`devboy doctor`, and (optionally) on `bootstrap`:

- **Format validation.** A `format_regex` in the index. Cheap, runs
  offline, catches typos and accidental `gh_xxx` vs `ghp_xxx` mixups.
- **Liveness validation.** Provider plugins (the existing API plugins
  under `crates/plugins/api/*`) expose a `test` method that performs
  a known-cheap authenticated call. Liveness is opt-in per secret;
  the validator looks up the provider from the path's second segment
  unless an explicit `validation` block names a different one.
- **Expiry and rotation tracking.** When the upstream API returns an
  expiry (GitLab and GitHub PATs do, for example), liveness validation
  records `expires_at` back into the global index. `rotate_every_days`
  paired with `last_rotated_at` produces advisory warnings. `doctor`
  surfaces "expires in N days" warnings under a fixed threshold
  (default seven days).

Validation never reads values from anywhere other than the credential
store, never logs them, and never writes them to the index.

### 7. Soft enforcement through manifest gating

The MCP API exposes `secrets.get(path)` (and `secrets.describe`,
`secrets.search`) for use by agents. By default, `secrets.get` resolves
a path **only if the path is declared in the manifest of the active
context**. A path that exists in the global keychain but is not declared
in the active manifest yields a structured error (`E_SECRET_NOT_IN_MANIFEST`)
rather than the secret value.

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

## Consequences

### Positive

- ✅ **Onboarding becomes data, not lore.** A new contributor runs
  `devboy secrets bootstrap`, the manifest drives an interactive walk
  through every required secret, and the system prompts use the
  global index's `retrieval_hint` to point at the right page.
- ✅ **Reuse without duplication.** A personal token is provisioned
  once and referenced from every project; rotation happens in one place.
- ✅ **Aliases in code, not values.** `@secret:<path>` makes plaintext
  in committed configuration a contradiction: a value next to the
  alias means someone bypassed the resolver.
- ✅ **Drift becomes visible.** A secret that expires, fails liveness,
  or is missing surfaces in `doctor`; a secret that is referenced
  with a non-conformant path fails validation at load time.
- ✅ **Manifest review.** A change to required secrets is a change
  to a tracked file, reviewable in a pull request.

### Negative

- ❌ **Two new files in the user's home directory.**
  `~/.devboy/secrets/index.toml` (metadata) is added on top of the
  existing `~/.devboy/config.toml`.
- ❌ **One new file in each project that opts in.** `.devboy/secrets.toml`
  is small but it is one more committed file.
- ❌ **Hard path convention is a one-way migration.** Existing
  keychain entries with names like `gitlab.token` are not valid paths
  under the new convention. ADR-005 entries continue to be readable
  through the legacy `CredentialStore` API, but the new manifest
  layer ignores them; a migration step is required (tracked in the
  implementation issue).
- ❌ **Authoring overhead for new secrets.** A new path requires an
  index entry before a manifest may reference it. The `bootstrap`
  flow asks for the necessary metadata interactively, but for a
  user accustomed to "just put a token in the keychain" this is one
  extra step.

### Risks

- ⚠️ **Manifest commits leak intent.** A committed manifest reveals
  which providers a project uses, even if no values leak. For a
  public OSS repository this is usually intended (it's part of the
  README); for a private repository describing a sensitive
  integration, the project may prefer to keep the manifest in a
  private companion repo. **Mitigation:** the manifest path is
  configurable; teams can place it outside the main repo if needed.
- ⚠️ **Alias-bypass via direct env-var read.** A library or tool
  that reads `process.env.GITLAB_TOKEN` directly (without going
  through the resolver) bypasses the alias system entirely.
  **Mitigation:** scope of this ADR is `devboy-tools`-mediated
  configuration; third-party tools using their own conventions are
  out of scope. CI checks (similar to ADR-019's `secrets-discipline`
  job) flag direct token reads inside `crates/`.
- ⚠️ **`@secret:` aliases in unmediated config.** A user copies an
  alias-bearing config snippet into a tool that does not understand
  `@secret:`, and the tool sends the literal string `@secret:...` as
  the credential. **Mitigation:** the alias prefix is documented and
  conspicuous; the wrapper tools explicitly fail closed when
  encountering an unresolved alias at exec time.
- ⚠️ **Validation false negatives mask real expiry.** If a provider
  API returns 200 even with a token that is two days from expiring,
  liveness validation does not catch it. **Mitigation:** rotation
  reminders driven by `last_rotated_at` and `rotate_every_days` are
  independent of liveness and run regardless of upstream behaviour.

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

### Alternative 3: No manifest, only the global index

**Description:** Drop the per-project manifest and let the index
itself declare which secrets each project needs.

**Why rejected:** The point of a per-project manifest is that it lives
**in the project's repository**, where it can be reviewed, tested in CI
("does this project still claim secrets it no longer uses?"), and used
to bootstrap new contributors. A central declaration in `~/.devboy/`
cannot fulfil any of those.

### Alternative 4: Encrypted file with embedded values (sealed manifest)

**Description:** Combine values and metadata into one file encrypted
with a per-user master key (age, sops, gpg).

**Why rejected:** A sealed file requires a master-key prompt on every
invocation (or a long-running agent process holding the key in
memory), which is exactly the ergonomic regression ADR-005 already
considered and rejected. ADR-005's keychain-plus-env-fallback model
remains the right place for values; this ADR layers metadata and
discovery on top of it.

### Alternative 5: Free-form path convention with linting only

**Description:** Allow any non-empty string as a path; lint deviations
from the recommended shape rather than rejecting them.

**Why rejected:** The current state (free-form `<provider>.<credential>`
keys per ADR-005) is exactly the lint-only baseline. Drift between
projects has already happened in practice. The hard rule is the
cheapest mechanism for preventing the namespace from re-fragmenting.

## Implementation

- **Issues:**
  - [#246](https://github.com/meteora-pro/devboy-tools/issues/246) — design (this ADR + ADR-021)
  - [#247](https://github.com/meteora-pro/devboy-tools/issues/247) — implementation, phased
- **Code (planned):**
  - `crates/devboy-storage/` — extend with manifest loader, global
    index, path validator
  - `crates/devboy-core/` — config-loader integration with
    `@secret:` resolution
  - `crates/devboy-executor/` — argv-substitution wrapper with
    stdin/FD pass-through
  - `crates/devboy-mcp/` — `secrets.*` tool surface, manifest gating,
    `Authorization` header rewriting in `proxy.rs`
  - `crates/devboy-cli/` — `devboy secrets {list, describe, validate,
    bootstrap}` subcommands
- **Migration:** legacy keychain entries from ADR-005 remain readable
  through the existing API; a migration tool walks them, asks the
  user for the canonical path under the new convention, and rewrites
  the index. Tracked as a separate phase in #247.

External secret sources, source routing, and per-context source
credentials are deferred to ADR-021.

## References

- [ADR-005: Credential storage](./ADR-005-credential-storage.md) — where
  values live (keychain + env fallback)
- [ADR-019: Secrets carry SecretString end-to-end](./ADR-019-secret-string-discipline.md)
  — how values are typed in transit through the process
- [ADR-021: External secret sources](./ADR-021-external-secret-sources.md)
  — pluggable backends (1Password, Vault, AWS, env-store, …) sitting
  behind the same path namespace
- [`secrecy` crate documentation](https://docs.rs/secrecy/)
- [HashiCorp Vault — KV namespace concepts](https://developer.hashicorp.com/vault/docs/secrets/kv) — inspiration for path-based addressing

---

## Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-05-06 | Andrei Mazniak | Initial draft |
