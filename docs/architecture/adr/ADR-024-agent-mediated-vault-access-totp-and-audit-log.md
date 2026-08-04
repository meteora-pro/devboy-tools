---
id: ADR-024
title: Agent-mediated vault access — TOTP unlock, configurable unlock window, liveness verdicts, and the encrypted audit log
status: proposed
date: 2026-08-04
deciders: ["Andrei Mazniak"]
tags: ["security", "secrets", "agent", "audit", "rotation", "core"]
supersedes: null
superseded_by: null
---

# ADR-024: Agent-mediated vault access — TOTP unlock, configurable unlock window, liveness verdicts, and the encrypted audit log

## Status

**proposed**

This is an **umbrella** ADR that extends [ADR-023](./ADR-023-secret-store-ux-layer.md),
in the same spirit as ADR-023's own umbrella over eight sub-decisions. The four
sub-decisions here are designed against each other: the ephemeral unlock
credential is what makes an agent-mediated unlock acceptable, the configurable
window is what makes daily agentic work practical, the liveness verdict is the
agent's only legitimate way to confirm a secret works, and the audit log is the
tamper-evident record of both agent activity and leak events. Splitting them
would multiply cross-references without adding clarity.

## Context

[ADR-023](./ADR-023-secret-store-ux-layer.md) ships the UX layer of the secret
framework: an encrypted local vault, a single-purpose daemon, a native TUI/GUI,
a manual-assisted rotation flow, a pattern catalogue, an MCP provisioning
protocol, and the `setup-secrets` onboarding skill. Its trust boundary is
strict and intentional: the agent surface never carries a secret value, the
unlock modal is agent-bypassing, and the daemon zeroizes the vault key after a
fixed 15-minute idle window.

Four gaps remain that ADR-023 does not close on its own.

1. **No agent-mediated unlock for the re-lock-during-session case.** ADR-023's
   daemon unlocks through a UI modal that the agent does not mediate, which is
   correct for the *initial* unlock (the agent is not running yet). But once an
   agent session is long-running, the daemon's idle re-lock fires, the next
   high-level provider call fails with `Locked`, and there is no in-band way
   for the agent to ask the user to re-unlock without the user leaving the
   terminal. Workflows that live entirely inside a non-graphical terminal (for
   example, inside a terminal multiplexer) have no surface for a GUI modal at
   all.

2. **The 15-minute idle re-lock is too aggressive for daily agentic work.** An
   agent session routinely runs for hours; a re-lock every 15 minutes turns
   into constant unlock friction. There is no way to say "I am working for the
   day, keep the vault unlocked until tonight."

3. **The agent cannot confirm a secret works without seeing it.** ADR-020 §6
   defines liveness validation, but it is exercised by `doctor` and `validate`,
   not exposed to the agent as a verdict-only operation. An agent that just
   provisioned a token through `request_provision` can only *infer* it works by
   trying a real provider call and reading the error.

4. **Agent activity and secret-leak events have no tamper-evident, encrypted
   store.** Today the agent's transcript is the only record of what it did, and
   a secret value that leaks into an agent-emitted log line, a tool result, or
   a downstream file has nowhere to be caught and recorded except an external
   sanitizer (#240) that does not yet exist in-tree. There is no append-only,
   vault-encrypted audit log that the agent can write to through a code path
   which *physically cannot* persist a raw value.

This ADR adds four sub-decisions, one per gap. Each is a narrow extension of an
ADR-023 component; none re-opens ADR-023's trust-boundary contract except where
this ADR states the relaxation explicitly and justifies it (sub-decision 3).

### Threat model alignment

This ADR inherits the threat model of ADR-020 / ADR-023: protection against
*accidental* leakage by humans, by agents acting in good faith, and by routine
tooling. It does **not** claim isolation against a malicious, shell-capable
agent. Sub-decision 3 (agent-mediated TOTP unlock) is a *deliberate, scoped
relaxation* of the "unlock is agent-bypassing" rule, justified solely by the
*ephemerality* of the relayed credential (a TOTP code is dead in ~30 s). The
relaxation applies to the unlock credential only; it never applies to secret
values, which remain on the ADR-023 invariant ("agent never sees value").

## Decision

> **Decision:** (1) Add a TOTP unlock envelope to the local vault. (2) Replace
> the fixed 15-minute idle re-lock with a configurable unlock window
> (`unlock_ttl`, bounded by a per-user `max_unlock_ttl`, optionally shortened
> by an idle safety). (3) Expose an agent-mediated `secrets_unlock(totp,
> duration?)` MCP tool — the one place an unlock credential crosses the agent
> surface — and a verdict-only `secrets_validate(path, liveness)` tool. (4) Add
> an encrypted, append-only audit log-store to the vault, written through a
> `vault_log_append` MCP tool whose server-side write path enforces a
> value→alias scrub, so a raw secret value is *physically incapable* of being
> persisted. (5) Make every agent-mediated write/rotate/delete reversible via
> per-path version history and soft-delete; permanent purge is a user-only
> action. (6) Deprecate the OS keychain/keyring dependency (ADR-005/021/023):
> the encrypted local vault, unlocked via passphrase/TOTP/recovery, becomes the
> primary cross-platform store, with no keychain dependency in the in-tree
> builtins.

### 0. Universality and vendor neutrality

`devboy-tools` is integration-agnostic and commercial-vendor-neutral (see CI
guard #243). This ADR defines **only** capabilities of the secret framework
itself — envelope kinds, a configurable daemon window, MCP tools, an on-disk
audit store, and version history. It specifies **no** dependency on any
particular agent, terminal, multiplexer, proxy, or companion product:

- The MCP tools speak the standard MCP tool surface; any MCP-compatible coding
  agent is an equal consumer.
- The daemon's TUI/GUI unlock (ADR-023 §3.4) and any terminal-based,
  non-graphical integration are interchangeable entry points for the same
  `vault.unlock` call.
- An agent-side convenience layer (a hook or skill authored in the agent's own
  configuration, a status indicator, a launcher wrapper) is **out of scope for
  this repository**. Such a layer adapts to the surfaces defined here; it does
  not leak back into them. Nothing in this ADR names or depends on a specific
  integration product.

A downstream integration that wants an in-band TOTP prompt, a status-bar glance
of vault state, or a launcher that resolves `@secret:` aliases implements it
entirely on its own side against the public MCP/daemon surface. The correctness
and security properties in this ADR hold regardless of which integration (if
any) is present.

### 1. TOTP unlock envelope

A new `Envelope::Totp` variant joins `Passphrase` / `Keychain` / `Recovery` in
`crates/devboy-vault-crypto/src/format.rs`. At enrollment (`devboy secrets
vault add-totp`) the CLI generates a random 32-byte TOTP secret, displays a
`otpauth://` QR (rendered in the TUI; as an ASCII QR or a bare secret in
headless mode). The `totp_secret` is stored in the **OS keystore** (macOS
Keychain / Windows DPAPI / Linux Secret Service — all per-user, **no `sudo`**),
and `vault_key` is wrapped under `HKDF(totp_secret)`. The `totp_secret` is
**never written to the vault file**, so possession of the file alone cannot
derive the wrap key — the Argon2id passphrase envelope remains the sole
at-rest protection, unchanged.

- **Algorithm:** TOTP per [RFC 6238](https://datatracker.ietf.org/doc/html/rfc6238),
  SHA-1, 6 digits, 30-second step (the universal default; interoperable with
  every authenticator app).
- **Storage & wrap (no at-rest regression):** the `totp_secret` lives in the OS
  keystore under a fixed account (e.g. `dev.devboy.secrets.totp`), accessed via
  the existing `keyring` crate. `vault_key` is AEAD-wrapped under
  `HKDF-Extract(totp_secret, "devboy-vault-totp")`. A 6-digit code is far too
  weak to derive a key, so the strong `totp_secret` — not the code — backs the
  wrap, and it resides only in the OS keystore, never on disk beside the
  ciphertext. This is the same pattern production TOTP-protected secret stores
  use, and it requires no `sudo` (the OS keystore is per-user).
- **Verification on unlock:** the daemon reads `totp_secret` from the OS
  keystore, recomputes the current TOTP, compares in constant time, and accepts
  one step of clock skew (±30 s). A match derives the wrap key and unwraps
  `vault_key`. Failed attempts are rate-limited (default: 5 per 30 s, then a
  60 s lockout) to bound brute force on the 6-digit space.
- **Fallback where no OS keystore is available** (CI, headless Linux without a
  Secret Service daemon): TOTP degrades to **session-scoped** — the daemon
  holds `totp_secret` in memory, established during the initial
  passphrase/Recovery unlock, and re-unlocks the vault in-session on re-lock.
  A fresh daemon start in this mode requires the passphrase once; the daily
  TOTP unlock is available only where the OS keystore is present.
- **Recovery relationship:** TOTP enrollment does **not** remove the passphrase
  envelope. A user who loses the authenticator still recovers via passphrase or
  BIP39 phrase. TOTP is a *convenience* unlock, not a recovery path.

### 2. Configurable unlock window

ADR-023 §3.3 fixes the unlocked state at "15 minutes of idle". This ADR
generalizes it to a user-controlled window while keeping the hard safety
guarantees (explicit lock, SIGTERM zeroize, process-exit zeroize).

- **`unlock_ttl`** (per-user, in `~/.devboy/config.toml` `[secrets]`). Default
  **8 hours** (a working day). The daemon holds `vault_key` for this duration
  after a successful unlock, enabling "unlock once in the morning".
- **`max_unlock_ttl`** (per-user hard ceiling). Default **24 hours**. A user may
  raise it (the threat model already grants a local process broad access), but
  the default keeps the window bounded.
- **`duration` parameter on `vault.unlock` / `secrets_unlock`.** Each unlock may
  request a specific window `≤ max_unlock_ttl`. Use case: "I am leaving a long
  task running overnight" → `duration = 24h`. Omitting `duration` uses
  `unlock_ttl`.
- **Idle safety (opt-in).** A separate `idle_relock` value (default **off**
  when `unlock_ttl` is set, to preserve the daily-unlock intent; can be turned
  on for defense-in-depth) re-locks after N minutes of *inactivity* even inside
  the unlock window. "Activity" is any successful `secret.get` or
  `metadata.update`, reusing the existing `IdleTracker`
  (`crates/devboy-secrets-agent/src/idle.rs`).
- **What does not change:** `vault.lock`, SIGTERM-trap zeroize within
  `SIGTERM_GRACE`, and process-exit drop of `vault_key` all remain. The window
  is a maximum, not a promise that the key survives the whole window.

The tradeoff is stated openly: a longer window means `vault_key` resides in
memory longer, widening the surface if the host is compromised while unlocked.
This is the user's explicit choice, bounded by `max_unlock_ttl`, and it is
consistent with ADR-023's threat model (which already accepts that a local
process can read the keychain and `/proc/self/environ`).

### 3. Agent-mediated unlock and liveness over MCP

Two new tools join the `secrets_*` family registered in
`crates/devboy-mcp/src/secrets_tool.rs`. Both honour the existing
`AgentSafeReply` marker — neither returns a secret value.

```
secrets_unlock(totp: string, duration?: number)
  → { unlocked: true, expires_at: timestamp }
  | { error: "BadTotp" | "RateLimited" | "WrongMethod" }
  // The agent relays a TOTP the user typed in chat. The daemon verifies
  // against the stored TOTP secret (constant-time, ±1 step) and unlocks for
  // `duration ?? unlock_ttl`, bounded by max_unlock_ttl.

secrets_status()
  → { state: "locked" | "unlocked", expires_at?, available_methods: [...] }

secrets_validate(path: string, liveness?: boolean)
  → { format: "ok" | "invalid", liveness?: "ok" | "invalid" | "unreachable", expires_at? }
  // Format check is offline. Liveness resolves the value server-side through
  // the source's validate() / the pattern's LivenessSpec, makes the cheap
  // authenticated call, and returns ONLY the verdict. The value never crosses
  // the MCP wire.
```

#### Why relaying a TOTP through the agent is acceptable

This is the single point at which this ADR relaxes ADR-023's "unlock is
agent-bypassing" rule. The justification is narrow and stated as a contract:

- **The relayed credential is ephemeral.** A TOTP code is valid for one 30 s
  step. A transcript that contains `428193` is useless to an attacker who reads
  it more than ~30 s later, unlike a master passphrase (persistent forever) or
  a secret value (persistent forever). The whole acceptability rests on this
  ephemeralness.
- **The master passphrase is never relayed.** The agent has no tool that
  accepts a passphrase; `secrets_unlock` takes a TOTP only. An agent that asks
  the user for the passphrase is not using this protocol and is out of scope.
- **The daemon rate-limits and binds to the local socket.** `secrets_unlock`
  rejects after 5 failed attempts per 30 s and verifies the peer UID on the
  UNIX socket (already required by ADR-023 §3.3). A replay requires both a
  fresh TOTP *and* local socket access.
- **Secret values are still never returned.** The relaxation is exclusively
  about the unlock credential. The `AgentSafeReply` invariant on values is
  untouched; the grep gate and negative test in ADR-023 §3.7 continue to apply.

One concrete UX (a non-graphical, terminal-based deployment): when a
high-level provider call fails with `Locked`, the agent prompts in its own
conversation — e.g. "vault locked, enter your TOTP" — relays the typed code to
`secrets_unlock`, and retries. No GUI modal, no separate window: the unlock
happens in-band. A companion agent-side integration (a hook or skill authored
in the agent's own configuration, not in this repository) can catch the
`Locked` error and inject the prompt automatically. The *initial* unlock of
the day, before the agent starts, still goes through a shell-level
TUI/biometric prompt (full security, no agent in the loop); the agent-mediated
path is only for the mid-session re-lock case.

### 4. Encrypted audit log-store with enforced value→alias scrub

A new append-only store lives alongside the vault entries, encrypted under the
same `vault_key`.

#### File layout

`~/.devboy/secrets/audit-log.dvb` (separate file, same key). Format mirrors
the vault's AEAD approach: a plaintext header (`AUDIT1`, version, entry count
for truncation detection) followed by contiguous per-entry ciphertexts, each

```
XChaCha20-Poly1305(
  plaintext = JSON { ts, session?, actor: "agent"|"user"|"daemon",
                     kind: "activity"|"leak"|"unlock"|"rotate"|...,
                     text, replaced?: [{ path, count }] },
  key       = vault_key,
  nonce     = entry.nonce,
  associated_data = "audit" utf-8 bytes || ts_bytes
)
```

Per-entry AEAD with `kind` and `ts` in AAD gives tamper evidence: a splice of
one entry's ciphertext under another's index fails decryption. There is no
whole-file Merkle tree; truncation is detected by the entry-count header, and
the threat model already grants single-writer (the daemon) and filesystem
permissions.

#### `vault_log_append` MCP tool and the enforced scrub

```
vault_log_append(text: string, kind?: string, session?: string)
  → { ok: true, replaced: [{ path, count }], stored_bytes: number }
  // The text the agent sent is NEVER persisted verbatim. Before encryption,
  // the daemon scrubs it.
```

The daemon's write path runs a **server-side reverse scrub** before encrypt +
append:

1. **Reverse match** the incoming `text` against every provisioned secret
   *value* the daemon holds in memory (it holds them while unlocked). Matching
   uses an Aho-Corasick automaton built from the current value set, so cost is
   O(text length) regardless of secret count.
2. **Replace** each match with its alias `@secret:<path>`.
3. **Audit** each replacement: append a `kind: "leak"` entry recording `{ path,
   count, where: "<calling tool>" }` — the leak event itself, never the value.
4. **Encrypt + append** the scrubbed text as `kind: "activity"` (or the
   caller-supplied kind).

The security property is structural, not conventional: **the agent cannot cause
a raw value to be persisted to the audit log**, because the scrub runs inside
the daemon's write path before encryption. An agent that intentionally sends
`glpat-abcdef…` finds `@secret:team/gitlab/token-deploy` in the stored entry.
This is the same enforcement posture as `AgentSafeReply` (a property of the
write path, not an honour system), lifted from "agent replies never leak
values" to "the audit log never stores values."

#### Relationship to the OTLP sanitizer (#240) and `otel scan` (#242)

All three consume `devboy-secret-patterns`. They differ in output:

- **#240 (OTLP sanitizer)** — detects secrets in *external* telemetry/logs,
  outputs `[REDACTED]`.
- **#242 (`otel scan`)** — detects secrets in existing OTEL artifacts, outputs
  findings for review.
- **This ADR (audit-log scrub)** — detects secrets in text the agent writes
  *into the vault*, outputs `@secret:<path>` (preserving *which* secret, for
  debuggability) plus a leak-audit entry.

The audit-log scrub additionally does **reverse lookup** (value → known path),
which #240/#242 cannot do (they see arbitrary text, not the provisioned value
set). When the scrub finds a value that matches no known provisioned secret but
*does* match a `SecretPattern` regex, it emits `[REDACTED:<pattern_id>]` (e.g.
`[REDACTED:gitlab-pat]`) and a leak-audit entry — catching unknown-but-shaped
tokens without inventing a path for them.

### 5. Secret versioning — agent edits are always reversible

A secret value is never destroyed by an agent-mediated write. Every
`secret.put` / `secret.rotate` appends a **new version** under the same path
rather than overwriting; resolution returns the newest non-tombstoned version.
The agent therefore cannot cause data loss by overwriting a token with a wrong
or empty value, or by rotating to a dud.

- **Versioned storage.** Each entry holds an ordered list of versions
  `{ version_id, ts, actor: "agent"|"user", value_ct, meta }`, each individually
  AEAD-encrypted (path + version_id as AAD, extending the §4.1 swap-attack
  protection to the version dimension). The on-disk layout is append-mostly: a
  new version is an append; a current-version pointer advances.
- **Soft delete only, for the agent.** `secret.delete` exposed to the agent
  writes a **tombstone** version: the path stops resolving (high-level provider
  tools fail with `NotProvisioned`), but every prior version remains on disk and
  recoverable. There is **no** agent-facing tool that purges a version.
- **Permanent purge is user-only.** Only an authenticated user action through
  the TUI/CLI (`devboy secrets purge <path>[@version]`, requiring the same fresh
  unlock as a write per ADR-023 §3.3) removes a version's ciphertext. **By
  default every version is retained indefinitely** (no auto-trim). A user may
  opt into trimming via `keep_versions` and/or `keep_days`; when set, versions
  older than the window are trimmed automatically, but the current version is
  never auto-trimmed.
- **Recovery.** `devboy secrets restore <path>[@version]` (user) or the TUI's
  history view points the current-version pointer back at a prior version. The
  value is never re-typed; the user picks from existing ciphertext versions.
- **Audit coupling.** Every `put` / `rotate` / `delete` (tombstone) / `restore`
  / `purge` appends an entry to the §4 audit log (`kind: "rotate"|"delete"|
  "restore"|"purge"`, with `version_id` and `actor`). Recovery is itself
  audited.

**Threat-model alignment.** Versioning protects against *accidental* damage by
a good-faith agent (the same class ADR-020 guards). It does not constrain a
malicious, shell-capable agent (out of scope, per ADR-020). Within the
framework's contract, however, the agent's writes and deletes are strictly
non-destructive: nothing an agent can do through the MCP surface is
irreversible, and only the user holds the irreversible operation (purge).

### 6. Deprecate the OS keychain/keyring — the vault is the primary store

With TOTP unlock (§1), a configurable window (§2), version history (§5), and
the encrypted local vault from ADR-023, the secret framework is usable as a
**standalone, cross-platform store with no dependence on the OS keychain or
Secret Service**. This ADR therefore deprecates the keychain/keyring dependency
that ADR-005 established and ADR-021 carried forward as a built-in source.

The motivation is portability and uniformity, not a claim that the vault is
cryptographically stronger than a hardware-backed keychain:

- The OS keychain is unavailable in CI runners, bare containers, headless Linux
  without a Secret Service daemon, and locked-down corporate Macs where
  keychain access prompts on every read. Every one of these forced a fallback
  path in ADR-005/021; the fallback became the common case.
- The vault (Argon2id KDF + XChaCha20-Poly1305 AEAD, ADR-023 §3.1) plus the
  TOTP/passphrase unlock is **identically available on every platform** the
  binary runs on — no D-Bus, no `Security` framework, no Windows Cred Manager.
- A single store means a single audit surface, a single rotation flow, and a
  single version-history implementation, instead of one per backend.

**What changes:**

- **ADR-021 `keychain` source is deprecated** as a built-in. `local-vault`
  becomes the `[default]` source and the recommended store for every path that
  is not explicitly routed to an external source (1Password, Vault, env-store).
  The `keychain` `SecretSource` implementation is removed from the in-tree
  builtins; a community plugin may reintroduce it for users who want it.
- **ADR-023 `Envelope::Keychain` (Touch ID unlock) is removed.** Unlock methods
  after this ADR are `Passphrase` / `Totp` / `Recovery` only. The macOS-only,
  keychain-backed biometric envelope is removed as a *per-secret* unlock path.
- **The OS keystore is retained for one narrow, opt-in role: the TOTP binding
  (§1).** The `keyring` crate stays a dependency so `totp_secret` can reside in
  the OS keystore (macOS Keychain / Windows DPAPI / Linux Secret Service,
  per-user, no `sudo`) rather than on disk. This is an *optional machine
  binding for the unlock secret*, not a primary secret store; where the OS
  keystore is absent (CI/headless Linux) TOTP falls back to session-scoped and
  the `keyring` dependency is simply unused. §6's intent — "the vault is the
  primary cross-platform store, no *requirement* on the OS keychain" — holds;
  the keychain is no longer required, only optionally reused for TOTP.
- **ADR-005 is superseded** in its "keychain as primary, env as fallback"
  decision. ADR-005's `SecretString` discipline and its env-store fallback
  remain (the env-store is still the CI source); only the keychain-as-primary
  role is replaced by the vault.

**Migration.** `devboy secrets migrate` (extended from ADR-020 §8) walks legacy
keychain entries, reads each value once (the one time keychain access is
required), writes it as the first version of the corresponding vault path, and
removes the keychain entry on explicit user confirmation. Until a user runs the
migration, the legacy keychain reader stays available behind a flag; after
migration, `[secrets] migration_complete = true` disables it.

**Threat-model tradeoff (stated openly).** Dropping the keychain removes the
OS-native hardware/biometric protection that a platform keychain provides. The
vault compensates with Argon2id (brute-force resistance on the passphrase), TOTP
(ephemeral agent-relayable unlock without exposing the passphrase), AEAD with
path-as-AAD (swap-attack resistance), and version history (reversibility). The
net posture is **portability and uniformity at the cost of OS-native
protection** — acceptable under ADR-020's threat model, which guards against
accidental leakage and explicitly does not claim isolation against a
shell-capable local process. Users who specifically need hardware-backed
protection can still install a community keychain source plugin (§6 of ADR-021
defines the plugin protocol) and route selected paths to it.

## Consequences

### Positive

- ✅ **Mid-session re-lock stops being fatal.** An agent running for hours can
  ask the user for a TOTP in-band and continue, without a GUI modal or leaving
  the terminal.
- ✅ **Daily unlock is one TOTP.** `unlock_ttl` defaulting to a working day
  removes the 15-minute re-lock friction while keeping an explicit ceiling.
- ✅ **The agent can confirm a secret works without seeing it.** `secrets_validate`
  returns a verdict; liveness stays server-side.
- ✅ **The audit log is tamper-evident and cannot store a raw value.** Agent
  activity and leak events get an encrypted, append-only record whose write
  path physically enforces value→alias scrubbing.
- ✅ **The pattern catalogue gains a third consumer**, alongside the planned
  #240 and #242, with a new reverse-lookup capability.
- ✅ **Agent edits are never destructive.** Every agent write/rotate/delete is
  reversible through version history; only the user can permanently purge, so a
  good-faith agent mistake (overwrite, delete) is always recoverable.
- ✅ **One store, every platform.** Deprecating the keychain removes the
  CI/headless/corporate-Mac fallback ladder and leaves a single cross-platform
  encrypted vault as the primary store.

### Negative

- ❌ **One more unlock method to maintain.** TOTP enrollment, QR rendering,
  drift handling, and rate-limiting are new surface in `devboy-vault-crypto`
  and the daemon.
- ❌ **A longer unlock window widens the in-memory exposure of `vault_key`.**
  Stated openly; bounded by `max_unlock_ttl`; the user's explicit choice.
- ❌ **Agent-mediated unlock is a trust-boundary exception.** Even though
  scoped to an ephemeral credential, it is one more thing to document and one
  more `secrets_*` tool whose contract must be auditable.
- ❌ **A new on-disk file** (`audit-log.dvb`) and a new `vault_log_*` tool
  family.
- ❌ **Loss of OS-native keychain protection.** Deprecating the keychain
  removes hardware/biometric backing on platforms that offer it; the vault's
  Argon2id + TOTP + AEAD compensates but is not a secure-element. Users who
  need it must install a community keychain source plugin.

### Risks

- ⚠️ **TOTP replay within the 30 s window.** An attacker with read access to
  the transcript *and* local socket access *and* acting within 30 s could
  replay the code. **Mitigation:** daemon rate-limiting, socket UID check, and
  the narrow 30 s window. The composite preconditions match the threat model's
  "local process" assumption.
- ⚠️ **Long unlock window + stolen laptop.** `vault_key` in RAM on a seized,
  unlocked machine. **Mitigation:** `max_unlock_ttl` ceiling, optional
  `idle_relock`, explicit `vault.lock`, and the documented posture that
  physical-end compromise is partly out of scope.
- ⚠️ **Reverse-scrub false negative for an unknown secret.** A value that is
  neither provisioned nor pattern-matched passes through unscrubbed.
  **Mitigation:** the `SecretPattern` regex catalogue (~30 patterns) catches
  the common shapes; unknown bespoke tokens are an accepted residual, surfaced
  by periodic `otel scan` (#242).
- ⚠️ **Audit log grows unbounded.** Append-only with no rotation fills disk.
  **Mitigation:** size-based rotation (drop oldest entries, re-encrypt the
  tail) with a configurable cap, plus a `devboy secrets audit export` command
  for archival.
- ⚠️ **Prompt injection via the TOTP prompt.** A malicious prompt could trick
  the agent into relaying a `secrets_unlock` call with a misleading reason.
  **Mitigation:** the dialog/prompt renders a fixed string ("vault locked,
  enter TOTP"); the user types the code, which is a 6-digit number with no
  semantic content to inject. There is no free-text channel from agent to the
  unlock decision beyond the fixed prompt.

## Alternatives Considered

### Alternative 1: Keep unlock strictly agent-bypassing; require a TUI popup on re-lock

**Description:** On mid-session re-lock, the daemon raises a terminal popup /
TUI modal; the agent simply fails with `Locked` until the user unlocks out of
band.

**Why rejected:** This works in a full interactive terminal but is fragile in
CI, in non-interactive agent runs (scheduled tasks), and in terminal
configurations where a popup is unwelcome. The agent-mediated TOTP path is the
in-band option that makes long agentic sessions practical; it is opt-in (a user
who wants strict agent-bypassing unlock simply does not enroll TOTP and keeps
the short idle window).

### Alternative 2: Passphrase relay instead of TOTP

**Description:** Let the agent relay the master passphrase to a `secrets_unlock(passphrase)`.

**Why rejected:** The passphrase is persistent. A transcript containing it is
compromised forever. TOTP's entire acceptability rests on ephemeralness; a
persistent credential does not have that property and must not cross the agent
surface.

### Alternative 3: Extend the idle timeout to a flat larger number

**Description:** Raise the 15-minute idle to, say, 8 hours and stop.

**Why rejected:** A flat constant ignores the two real needs — daily unlock
(`unlock_ttl`) and "I am leaving for the night" extension (`duration`). The
configurable window with a ceiling and an optional idle safety is strictly more
expressive at trivial extra cost.

### Alternative 4: Audit log as plaintext, scrubbed client-side by the agent

**Description:** The agent scrubs its own log text before sending; the log is
plaintext.

**Why rejected:** Client-side scrub is honour-system. The whole point of the
audit log is a tamper-evident record of *what really happened*, including the
agent's mistakes; trusting the agent to scrub itself defeats the purpose.
Server-side enforced scrub is the load-bearing property.

### Alternative 5: Audit log stored in an external system (OTLP/SIEM) instead of the vault

**Description:** Ship agent activity and leak events to an existing log
backend; do not add a vault-local store.

**Why rejected:** Useful as an *export* destination (`audit export`, future
work), but the in-vault encrypted store is the one place guaranteed to share
the secret framework's security boundary and to be available in fully offline
headless deployments with no SIEM. Export is additive, not a replacement.

## Implementation

- **Issues:**
  - This ADR: to be filed as the tracking issue (see the issue draft
    `agent-mediated-vault-access-and-audit-log`).
  - [#240](https://github.com/meteora-pro/devboy-tools/issues/240) — OTLP
    sanitizer; shares `devboy-secret-patterns`.
  - [#242](https://github.com/meteora-pro/devboy-tools/issues/242) — `otel
    scan`; shares `devboy-secret-patterns`.
  - [#247](https://github.com/meteora-pro/devboy-tools/issues/247) — secrets
    implementation, phased (this ADR is a new phase cluster on top of it).
- **Code (planned):**
  - `crates/devboy-vault-crypto/src/format.rs` — `Envelope::Totp` variant
    (currently `Passphrase`/`Keychain`/`Recovery` at line 349); TOTP
    verify + envelope wrap.
  - `crates/devboy-secrets-agent/src/idle.rs` — generalize
    `DEFAULT_IDLE_TIMEOUT` / `IdleTracker` to a configurable window
    (`unlock_ttl`, `max_unlock_ttl`, optional `idle_relock`); add TOTP
    verification + rate-limit to the unlock path.
  - `crates/devboy-secrets-agent/` — new `audit_log` module: append-only
    AEAD store, reverse-scrub write path (Aho-Corasick over provisioned values
    + `SecretPattern` regex fallback), leak-audit entries.
  - `crates/devboy-mcp/src/secrets_tool.rs` — register
    `secrets_unlock`, `secrets_status`, `secrets_validate`,
    `vault_log_append`; all return `AgentSafeReply` (no value).
  - `crates/devboy-secret-patterns/` — new reverse-lookup helper
    (`build_scrubber(values) -> Scrubber`) consumed by the audit-log write path.
  - `crates/devboy-cli/src/secrets_cmd.rs` — `devboy secrets vault add-totp`,
    `audit {export,tail}`, `validate --liveness`, `restore <path>[@version]`,
    `purge <path>[@version]` (user-only).
  - `crates/devboy-vault-crypto/src/format.rs` + `crates/devboy-secrets-agent/` —
    per-path version list (append + current-pointer), tombstone-on-delete,
    user-only purge, retention trimming; the MCP `secret.delete` becomes a
    tombstone write, never a ciphertext removal.
  - `crates/plugins/secrets/keychain/` — **remove** the in-tree keychain source;
    `local-vault` becomes the `[default]` source. `crates/devboy-vault-crypto/`
    — remove `Envelope::Keychain`; unlock methods become
    `Passphrase`/`Totp`/`Recovery`. `devboy secrets migrate` — extend to read
    each legacy keychain entry once and write it as a vault version, with the
    `[secrets] migration_complete` flag disabling the legacy reader afterward.
- **Documentation (planned):**
  - `docs/guide/secrets/agent-protocol.md` — add the new tools.
  - `docs/guide/secrets/local-vault.md` — TOTP enrollment, unlock-window
    configuration, audit-log rotation, version history and recovery.
  - A new BDD scenario `docs/guide/secrets/scenarios/totp-unlock-and-audit.feature`.

## References

- [ADR-019](./ADR-019-secret-string-discipline.md) — `SecretString` end-to-end.
- [ADR-020](./ADR-020-secret-manifest-and-alias-resolution.md) — manifest, path
  namespace, alias resolution (`@secret:<path>`), validation (§6 liveness).
- [ADR-021](./ADR-021-external-secret-sources.md) — source router and
  `SecretSource::validate`.
- [ADR-023](./ADR-023-secret-store-ux-layer.md) — local vault, daemon,
  `AgentSafeReply`, the 15-minute idle policy this ADR generalizes.
- [RFC 6238](https://datatracker.ietf.org/doc/html/rfc6238) — TOTP.
- [RFC 8439](https://datatracker.ietf.org/doc/html/rfc8439) — ChaCha20-Poly1305.

---

## Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-08-04 | Andrei Mazniak | Initial draft — TOTP unlock envelope, configurable unlock window, agent-mediated `secrets_unlock` / `secrets_validate`, encrypted audit log-store with enforced value→alias scrub. |
