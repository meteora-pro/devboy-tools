---
id: ADR-024
title: Agent-mediated vault access — TOTP re-unlock, configurable unlock window, liveness verdicts, encrypted audit log, versioning, keychain demotion, the trusted-path process model, and actionable agent errors
status: proposed
date: 2026-08-04
deciders: ["Andrei Mazniak"]
tags: ["security", "secrets", "agent", "audit", "rotation", "trusted-path", "core"]
supersedes: null
superseded_by: null
---

# ADR-024: Agent-mediated vault access — TOTP re-unlock, audit log, versioning, keychain demotion, trusted path, and actionable errors

## Status

**proposed**

This is an **umbrella** ADR that extends [ADR-023](./ADR-023-secret-store-ux-layer.md),
in the same spirit as ADR-023's own umbrella over eight sub-decisions. The eight
sub-decisions here are designed against each other: the vault-resident TOTP
secret is what lets an agent-mediated re-unlock prove human presence without
any OS keystore; the configurable window is what makes daily agentic work
practical; the liveness verdict is the agent's only legitimate way to confirm a
secret works; the audit log is the tamper-evident record of agent activity and
leak events; per-path version history makes every agent edit reversible;
demoting the OS keychain to an opt-in makes the vault a single cross-platform
store; the trusted-path process model is what makes all six of the above
meaningful in the presence of a same-UID agent; and actionable errors are what
let an agent act on all seven correctly instead of guessing. Splitting them
would multiply cross-references without adding clarity.

The last two are load-bearing and easy to miss. An unlock method is only as
strong as the process that collects it: §1–§6 describe *what* is stored and
*how* it is unlocked, while §7 describes *who* may observe the unlock — without
it, §1–§6 protect the vault from everything except the one adversary this ADR
is actually about. And a guarantee an agent cannot navigate is a guarantee it
routes around: §8 makes every failure carry its own remedy, including the
explicit signal that only a human can proceed from here.

## Context

[ADR-023](./ADR-023-secret-store-ux-layer.md) ships the UX layer of the secret
framework: an encrypted local vault, a single-purpose daemon, a native TUI/GUI,
a manual-assisted rotation flow, a pattern catalogue, an MCP provisioning
protocol, and the `setup-secrets` onboarding skill. Its trust boundary is
strict and intentional: the agent surface never carries a secret value, the
unlock modal is agent-bypassing, and the daemon zeroizes the vault key after a
fixed 15-minute idle window.

Six gaps remain that ADR-023 does not close on its own.

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

5. **Every unlock method is collected by a process the agent can replace.**
   ADR-023 calls the unlock modal "agent-bypassing", but nothing in the design
   makes it so. The daemon, the CLI, and the agent all run under the **same
   UID**, and the passphrase prompt lives inside the CLI process
   (`dialoguer::Password` in `crates/devboy-cli/src/secrets_cmd.rs`). An agent
   that can run a shell — which is the normal case, not an exotic one — can
   replace the `devboy` binary on `PATH`, edit the shell rc file, or set
   `LD_PRELOAD`, and harvest the passphrase the next time the user types it.
   No choice of unlock *method* fixes this: passphrase, keyfile, TOTP, and
   hardware token are all equally exposed when the collecting process is
   untrusted. This is a gap in the **process model**, not in the cryptography,
   and it silently weakens all four gaps above.

6. **Errors say what failed, never what to do about it.** Every failure the
   framework can return — `Locked`, `NotProvisioned`, an expired token, a
   pending approval — reaches the agent as a bare code. The framework knows the
   correct next step in each case, and in several of them the correct step is
   *stop and fetch the user*; none of that is expressed. An agent left to infer
   it will guess, and the plausible guesses are actively harmful: asking the
   user for the master passphrase, starting the daemon itself (which voids gap
   5's fix), hunting for the value in the environment, or retrying until the
   rate limiter locks the user out. A guarantee an agent cannot navigate is a
   guarantee it routes around.

This ADR adds eight sub-decisions — six addressing the gaps above (§1–§4, §7,
§8), plus per-path version history (§5) and the OS keychain/keyring demotion
(§6). Each is a narrow extension of an ADR-023 component; none re-opens
ADR-023's trust-boundary contract except where this ADR states the relaxation
explicitly and justifies it (sub-decision 3).

### Threat model alignment

This ADR inherits the threat model of ADR-020 / ADR-023: protection against
*accidental* leakage by humans, by agents acting in good faith, and by routine
tooling. It does **not** claim isolation against a malicious, shell-capable
agent that has already achieved persistence on the machine.

Within that boundary this ADR **raises** the bar in one specific direction:
the agent is treated as an *untrusted same-UID process* rather than as a
trusted part of the toolchain. Three consequences follow, and they shape §1,
§3 and §7:

- **The agent must never hold `vault_key`.** It holds a socket connection and
  asks the daemon; the daemon decides. This is already ADR-023's design, but
  it only holds if the daemon is not readable by the agent — see §7.
- **Ephemerality alone does not justify relaying a credential.** An earlier
  draft of this ADR justified `secrets_unlock(totp)` by the ~30 s lifetime of
  a TOTP code. That reasoning is insufficient on its own: a credential the
  agent can *derive* is not a credential at all, however short-lived. The
  actual justification is that the TOTP secret lives where the agent cannot
  reach it (§1), which makes a valid code **evidence of human presence**. The
  30 s window bounds replay; it does not create the guarantee.
- **The relaxation applies to the unlock credential only.** It never applies
  to secret values, which remain on the ADR-023 invariant ("agent never sees
  value").

What remains explicitly out of scope: an agent that has already replaced the
binaries or the shell environment can wait for a legitimate unlock and act
inside the window. §7 states this limit rather than papering over it.

## Decision

> **Decision:** (1) Add a TOTP **re-unlock** method whose secret lives inside
> the encrypted vault and is held in daemon memory after a passphrase unlock —
> no OS keystore, and not a replacement for the passphrase. (2) Replace the
> fixed 15-minute idle re-lock with a configurable unlock window (`unlock_ttl`,
> bounded by a per-user `max_unlock_ttl`, optionally shortened by an idle
> safety), shipped as two named profiles rather than one default. (3) Expose an
> agent-mediated `secrets_unlock(totp, duration?)` MCP tool — the one place an
> unlock credential crosses the agent surface — and a verdict-only
> `secrets_validate(path, liveness)` tool. (4) Add an encrypted, append-only
> audit log-store to the vault, written through a `vault_log_append` MCP tool
> whose server-side write path enforces a value→alias scrub, so a raw secret
> value is *physically incapable* of being persisted. (5) Make every
> agent-mediated write/rotate/delete reversible via per-path version history
> and soft-delete; permanent purge is a user-only action. (6) **Demote** the OS
> keychain/keyring (ADR-005/021/023) from primary store to an **opt-in**
> source, disabled by default on every platform: the encrypted local vault is
> the default store, and the in-tree keychain source stays available behind an
> explicit setting. Guarantee a **first-class env-only mode** for CI,
> containers and headless hosts, in which the environment is the sole source
> and no vault, daemon, keychain or prompt is involved — under both the
> ADR-005 and ADR-021 variable-naming conventions, so existing pipelines keep
> working. (7) Adopt a **trusted-path
> process model**: the daemon runs under its own UID, collects the passphrase
> and per-call approvals itself, and must not be a child of the agent — because
> no unlock method is stronger than the process that collects it. (8) Make
> every error **actionable**: each failure reply carries a `remediation` object
> naming who can resolve it (`agent` or `user`), a machine-readable next step,
> and daemon-authored text to show the human — so the agent asks the user for
> help when only a human can proceed, and never guesses its way around the
> guarantees above.

### 0. Universality and vendor neutrality

`devboy-tools` is integration-agnostic and commercial-vendor-neutral (see CI
guard #243). This ADR defines **only** capabilities of the secret framework
itself — envelope kinds, a configurable daemon window, MCP tools, an on-disk
audit store, version history, and the daemon's own process model. It specifies
**no** dependency on any particular agent, terminal, multiplexer, proxy, or
companion product:

- The MCP tools speak the standard MCP tool surface; any MCP-compatible coding
  agent is an equal consumer.
- The daemon's TUI/GUI unlock (ADR-023 §3.4) and any terminal-based,
  non-graphical integration are interchangeable entry points for the same
  `vault.unlock` call. §7 constrains *where the credential is collected*, not
  which front-end triggers the unlock: any integration may ask the daemon to
  begin an unlock, but none of them may collect the passphrase on its behalf.
  That constraint is vendor-neutral — it names a process boundary, not a
  product.
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

### 1. TOTP re-unlock — a vault-resident secret, not a keystore binding

#### Why TOTP cannot replace the passphrase

An earlier draft of this ADR treated TOTP as a *convenience unlock*: type six
digits in the morning instead of a long passphrase. That framing is wrong, and
the reason is worth stating as a rule, because it generalizes to any
"convenient" unlock method:

> The strength of a TOTP unlock equals the strength of wherever `totp_secret`
> is stored — **never** the ~20 bits of the code.

A code is always derivable from the secret. So a TOTP unlock is at best as
strong as its secret store, and never stronger than the passphrase that
protects the vault itself. If `totp_secret` sits anywhere an attacker (or an
agent) can read — a keyfile, a world-readable keystore, a plaintext blob beside
the vault — then TOTP contributes **exactly zero** additional protection
against that attacker: they compute the code themselves.

This is not hypothetical. On Linux the Secret Service hands a stored secret to
**any process in the user's session** with no per-application ACL, which is the
same protection as `chmod 0600`. Binding `totp_secret` to that keystore and
calling the result a second factor would be theatre.

The passphrase therefore remains the **only** cold-start unlock method
(alongside Recovery, and the opt-in keyfile of §6). TOTP is repositioned to the
one job it can actually do.

#### What TOTP is for: proving human presence to a same-UID agent

The real problem TOTP solves is gap 1 + gap 5 together: an agent session runs
for hours, the daemon re-locks, and the agent needs the vault back — but the
agent is exactly the party we do not want deciding that. A TOTP code the agent
**cannot derive** is evidence that a human, holding a second device, approved
the re-unlock.

That property requires `totp_secret` to be somewhere the agent cannot read. It
does not require an OS keystore — and given the Linux behaviour above, an OS
keystore would not deliver it anyway. Instead:

> `totp_secret` is stored **inside the encrypted vault** and lives in **daemon
> memory** after a passphrase unlock. It never exists in plaintext on disk, and
> the agent cannot read the daemon's memory (§7).

#### Lifecycle

```
cold start (boot / daemon restart)
  passphrase (Argon2id)  →  vault_key  →  read totp_secret  →  hold in daemon RAM

re-lock (unlock_ttl expiry, idle_relock, explicit vault.lock)
  zeroize vault_key      →  totp_secret STAYS in RAM

re-unlock (agent-mediated, §3)
  6 digits  →  verify against RAM copy  →  HKDF(totp_secret)  →  unwrap vault_key
```

There is no circular dependency: `Envelope::Totp` is only ever consulted for a
*re*-unlock, at which point `totp_secret` is already resident. A daemon that has
never been unlocked in this boot has no TOTP path — by design.

- **Envelope.** `Envelope::Totp { totp_salt, wrapped_key }` joins `Passphrase`
  and `Recovery` in `crates/devboy-vault-crypto/src/format.rs`. (It does *not*
  join `Keychain`, which §6 removes.) Adding an envelope is a header-only write
  that never touches per-entry ciphertext, as the existing module doc states.
- **Algorithm.** TOTP per [RFC 6238](https://datatracker.ietf.org/doc/html/rfc6238),
  SHA-1, 6 digits, 30-second step — the universal default, interoperable with
  every authenticator app.
- **Wrap.** `vault_key` is AEAD-wrapped under a key derived via HKDF-SHA256:
  `Hkdf::<Sha256>::new(Some(totp_salt), totp_secret)` then
  `.expand(b"devboy-vault-totp-key-v1", &mut out)` — mirroring
  `recovery::derive_recovery_key`. The strong 32-byte secret backs the wrap;
  the 6-digit code only gates access to it.
- **Reserved storage slot.** `totp_secret` is held as a vault entry under a
  reserved path (`__totp/secret`) that is **not resolvable through
  `secret.get`, not listed by `secrets_list`, and not exportable** — following
  the existing `__sources/` convention for source-credentials
  (`crates/devboy-storage/src/router_credentials.rs`). Without this the whole
  scheme collapses: an agent would simply ask the unlocked daemon for the TOTP
  secret and mint its own codes.
- **Replay guard.** The daemon records the last accepted time-step and rejects
  any code from that step or earlier (RFC 6238 §5.2). A code the agent
  observed while relaying it cannot be reused within its own window.
- **Rate limiting.** 5 failed attempts per 30 s, then a 60 s lockout, bounding
  brute force over the 6-digit space.
- **Clock skew.** ±1 step (±30 s) accepted, subject to the replay guard.
- **Enrollment.** `devboy secrets vault add-totp` requires an unlocked vault,
  generates a random 32-byte secret, writes it to `__totp/secret`, creates
  `Envelope::Totp`, and displays an `otpauth://` QR (TUI QR, ASCII QR, or bare
  secret in headless mode) for the user's authenticator app.
- **Recovery relationship.** Enrollment never removes the passphrase envelope.
  A user who loses the authenticator still unlocks by passphrase or BIP39
  phrase. TOTP is neither a recovery path nor a cold-start path.
- **Dependency on §7.** This entire sub-decision rests on the agent being
  unable to read daemon memory. If the daemon runs under the agent's UID *and*
  as its child, `ptrace` is permitted under the common
  `kernel.yama.ptrace_scope = 1` policy and `totp_secret` is readable. **A
  lazily-started daemon spawned by the agent must not be trusted for the TOTP
  path** — see §7.

#### What this does not claim

TOTP does not protect against an agent that has already replaced binaries and
waits for the user to unlock legitimately; it gates the *transition* from
locked to unlocked, not the window that follows. That limit belongs to §2's
profile choice and §7's honest boundary.

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
- **`duration` parameter on `vault.unlock` / `secrets_unlock` (seconds).** Each
  unlock may request a specific window `≤ max_unlock_ttl`, expressed in seconds
  to match the rest of the MCP surface (e.g. `age_seconds`, `ttl_seconds` in
  ADR-023 §3.7). Use case: "I am leaving a long task running overnight" →
  `duration = 86400` (24 h). Omitting `duration` uses `unlock_ttl`.
- **Idle safety (opt-in).** A separate `idle_relock` value (default **off**
  when `unlock_ttl` is set, to preserve the daily-unlock intent; can be turned
  on for defense-in-depth) re-locks after N minutes of *inactivity* even inside
  the unlock window. "Activity" is any successful `secret.get` or
  `metadata.update`, reusing the existing `IdleTracker`
  (`crates/devboy-secrets-agent/src/idle.rs`).
- **What does not change:** `vault.lock`, SIGTERM-trap zeroize within
  `SIGTERM_GRACE`, and process-exit drop of `vault_key` all remain. The window
  is a maximum, not a promise that the key survives the whole window.

#### Two profiles, because one default cannot serve both goals

A long unlock window and a strong anti-agent posture pull in opposite
directions, and this ADR refuses to hide that behind a single default. A wide
window is what makes daily agentic work bearable; it is also precisely the
window in which a compromised agent operates freely, because §7's trusted path
protects the *transition* into the unlocked state, not the duration of it.

Two named profiles are therefore shipped, selected by
`[secrets] profile = "convenient" | "strict"`:

| | `convenient` (default) | `strict` |
|---|---|---|
| `unlock_ttl` | 8 h | 15 min |
| `max_unlock_ttl` | 24 h | 1 h |
| `idle_relock` | off | 5 min |
| `approve_on_use` floor | honour per-path setting | forced to `per-call` |
| approval UI | daemon-rendered (§7) | daemon-rendered (§7) |
| intended for | a developer laptop running a trusted agent | shared hosts, high-value paths, untrusted or unattended agents |

`strict` is not merely "smaller numbers": forcing `approve_on_use` to
`per-call` is what turns each secret access into a human decision, which is the
only mitigation for the "agent waits for a legitimate unlock" attack. A user may
still override any individual value; the profiles exist so that the *coherent*
combinations are one setting away rather than four.

**Both profiles require a human at a surface the daemon can reach.** `strict`
depends on rendering an approval per access, so it is unavailable where the
daemon has no way to ask — a headless server with no TTY and no display. Such
a deployment must choose deliberately between `convenient` (accepting that an
unlock covers everything until it expires) and the env-only mode of §6 (no
vault at all). Selecting `strict` where no prompt surface exists must fail at
configuration time with that explanation, not at the first secret access in the
middle of a job.

**TOTP (§1) belongs to `convenient`.** The two sub-decisions interact in a way
worth stating plainly, because the naive reading is backwards: under
`convenient` the vault re-locks roughly once a working day, so a TOTP re-unlock
costs one glance at a phone and is exactly the right instrument. Under `strict`
the vault re-locks every 15 minutes, and a TOTP challenge at that cadence is
unusable — that profile's answer to a re-lock is a fresh passphrase entry
through the §7 prompt, or simply doing the work in a shorter session. TOTP
enrollment is therefore recommended with `convenient` and neither required nor
prohibited with `strict`.

The tradeoff is stated openly: a longer window means `vault_key` resides in
memory longer, widening the surface if the host is compromised while unlocked.
This is the user's explicit choice, bounded by `max_unlock_ttl`, and it is
consistent with ADR-023's threat model (which already accepts that a local
process can read `/proc/self/environ`).

### 3. Agent-mediated unlock and liveness over MCP

Two new tools join the `secrets_*` family registered in
`crates/devboy-mcp/src/secrets_tool.rs`. Both honour the existing
`AgentSafeReply` marker — neither returns a secret value.

```
secrets_unlock(totp: string, duration?: number)   // duration in seconds, ≤ max_unlock_ttl
  → { unlocked: true, expires_at: timestamp }
  | { error: "BadTotp" | "ReplayedCode" | "RateLimited"
            | "TotpUnavailable" | "NotAvailableInCiMode" }
  // The agent relays a TOTP the user typed in chat. The daemon verifies
  // against the in-memory TOTP secret (constant-time, ±1 step, spent steps
  // rejected) and unlocks for `duration ?? unlock_ttl`, bounded by
  // max_unlock_ttl.
  //
  // TotpUnavailable is the case the agent will actually hit: the daemon
  // restarted, so totp_secret is no longer resident and only a passphrase can
  // open the vault. It is a distinct error precisely so the agent stops asking
  // for codes the daemon cannot check, and tells the user to unlock through
  // the §7 prompt instead. The reply carries the reason
  // ("no TOTP session this boot" / "TOTP not enrolled") but no further detail.

secrets_status()
  → { state: "locked" | "unlocked", expires_at?, available_methods: [...],
      trust_level: "separate_uid" | "independent" | "agent_parented" | "env_only" }
  // available_methods reflects what will actually work right now — it omits
  // "totp" when no TOTP session is resident OR when the daemon's self-check
  // (§7) found the caller in its own ancestry, so a well-behaved agent can
  // check before prompting the user for a code that cannot succeed.
  //
  // trust_level is computed by the daemon from its actual process layout, not
  // asserted by configuration. An agent that reports vault state to the user
  // should surface it: "unlocked, but the daemon cannot protect its memory
  // from this session" is materially different from "unlocked".

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

- **The agent cannot derive the credential.** This is the load-bearing
  property, and it comes from §1: `totp_secret` lives inside the encrypted
  vault and in daemon memory, never in a place the agent can read. A valid code
  therefore constitutes *evidence that a human with a second device approved
  the re-unlock*. Relaying a credential the agent could compute itself would be
  meaningless regardless of how briefly it lived.
- **Ephemerality bounds replay; it does not create the guarantee.** A code is
  valid for one 30 s step, and the daemon's replay guard (§1) rejects a step
  that was already spent — so an agent that observed the code while relaying it
  cannot reuse it even inside its own window. This limits blast radius; it is
  not the reason the scheme is sound.
- **The master passphrase is never relayed.** The agent has no tool that
  accepts a passphrase; `secrets_unlock` takes a TOTP only. An agent that asks
  the user for the passphrase is not using this protocol and is out of scope —
  and under §7 the passphrase prompt is not something the agent can render
  convincingly in the first place.
- **The daemon rate-limits and binds to the local socket.** `secrets_unlock`
  rejects after 5 failed attempts per 30 s and verifies the peer UID on the
  UNIX socket (already implemented, `crates/devboy-secrets-agent/src/socket.rs`).
- **Secret values are still never returned.** The relaxation is exclusively
  about the unlock credential. The `AgentSafeReply` invariant on values is
  untouched; the grep gate and negative test in ADR-023 §3.7 continue to apply.

One concrete UX (a non-graphical, terminal-based deployment): when a
high-level provider call fails with `Locked`, the agent prompts in its own
conversation — e.g. "vault locked, enter your TOTP" — relays the typed code to
`secrets_unlock`, and retries. No GUI modal, no separate window: the unlock
happens in-band. A companion agent-side integration (a hook or skill authored
in the agent's own configuration, not in this repository) can catch the
`Locked` error and inject the prompt automatically.

The *cold-start* unlock, before the agent starts, goes through the
daemon-rendered passphrase prompt of §7 — no agent in the loop, and no process
the agent can substitute. The agent-mediated path exists only for the
mid-session re-lock case, and only after a human has already established
`totp_secret` in daemon memory by unlocking with the passphrase at least once
this boot.

### 4. Encrypted audit log-store with enforced value→alias scrub

A new append-only store lives alongside the vault entries, encrypted under the
same `vault_key`.

#### File layout

`~/.devboy/secrets/audit-log.dvb` (separate file, same key). Format mirrors
the vault's AEAD approach: a plaintext header (`AUDIT1`, version, entry count
for truncation detection), a plaintext per-entry index (`seq → { nonce,
ct_offset }`, so each entry's sequence number and nonce are available without
decrypting the body), followed by contiguous per-entry ciphertexts, each

```
XChaCha20-Poly1305(
  plaintext = JSON { ts, session?, actor: "agent"|"user"|"daemon",
                     kind: "activity"|"leak"|"unlock"|"rotate"|...,
                     text, replaced?: [{ path, count }] },
  key       = vault_key,
  nonce     = entry.nonce,
  associated_data = b"audit-v1" || seq_bytes   // seq from the plaintext index
)
```

`ts` lives inside the encrypted JSON (it is not trusted as AAD — AAD must be
available verbatim at decrypt time). Per-entry AEAD with the plaintext
sequence number in AAD gives tamper evidence: a splice of one entry's
ciphertext under another's index fails decryption, because the AAD `seq` no
longer matches. There is no
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

#### The other direction: the agent's own transcript (Ф15)

The scrub above protects what the agent *writes into* the vault. The larger
exposure runs the other way — what devboy *hands back to* the agent.

An agent session transcript is a JSONL file on disk. Every tool result is
appended to it verbatim and kept indefinitely, and any process running as the
user can read it. So the question is not whether devboy stores a value, but
whether one can pass *through* devboy into that file.

The routes were audited exhaustively:

| Route | Verdict |
|---|---|
| devboy's own MCP tool replies | Clean. No tool returns a value; `AgentSafeReply` fences the reply structs. |
| CLI output | Clean. One place prints a secret and it is masked. |
| Error text built by devboy | Clean. Messages name the path, the pattern or the regex — never the value. |
| **Proxied upstream tool results** | **Was open.** Returned to the agent verbatim. |
| **Proxied upstream transport errors** | **Was open**, and by a different route: a non-2xx never becomes a result inside the proxy client at all — it becomes an error carrying the response body, which the proxy manager formats into a result further up. |

Both proxied routes are now scrubbed at `McpProxyClient`, over two passes:
credentials this process sent upstream (the connect-time bearer or API key, and
the current OAuth access token, registered at the moment of sending so a
post-401 refresh is covered), and the pattern catalogue for anything
secret-shaped that devboy has never seen.

Three properties are deliberate:

- **Nothing new is loaded to do it.** The registry labels material already in
  the MCP server's memory. Pulling every provisioned secret into that process so
  it could recognise them would create a larger exposure than the one being
  closed.
- **No opt-out, no per-upstream allow-list.** A tool that genuinely means to
  return a token will show `[REDACTED:jwt]`. That is consistent with ADR-020 —
  agents work with aliases, not values — and the redaction is visible rather
  than silent, so the rare user it inconveniences can see exactly what happened.
- **It does not write to the audit log.** The log lives in the daemon and this
  runs in the MCP server; routing every proxied response through an RPC would
  put the daemon on the hot path of every tool call. Leaks are reported through
  `tracing`, naming the secret and never the value.

Note the dependency on the catalogue being able to match *inside* a string. The
catalogue's regexes are anchored validators (`^glpat-…$`), which answer "is this
whole string a token?" and can never find one mid-sentence. A pattern is
therefore promoted to a scanning form only when it has a literal prefix and no
unbounded wildcard; the generic `^[A-Za-z0-9._-]{40,}$` catch-all and the four
connection-string patterns are refused, because unanchored they would match
commit hashes, base64 blobs and the remainder of any JSON line. Those five keep
whole-string validation.

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

### 6. Demote the OS keychain/keyring to an opt-in — the vault is the default store

With the passphrase unlock, a configurable window (§2), version history (§5),
and the encrypted local vault from ADR-023, the secret framework is usable as a
**standalone, cross-platform store with no dependence on the OS keychain or
Secret Service**. This ADR therefore **demotes** the keychain from the primary
store that ADR-005 established — but does **not** remove it from the tree.

An earlier draft removed the in-tree `keychain` source outright. That is the
wrong instrument: removal forces users who legitimately want OS-native backing
onto a community plugin, and it collides with §7, where the macOS keychain
turns out to provide something nothing else in this design does. The correct
instrument is a default flip plus an explicit setting.

#### What the keychain actually buys, per platform

The decision is easier once the marketing is stripped away. "OS keychain" names
three quite different mechanisms:

| Platform | Backend | Who can read it | Effective strength |
|---|---|---|---|
| **macOS** | Keychain Services | only processes matching the item's ACL — **bound to code signature** — otherwise a user prompt | **Stronger than a file.** The only anti-tamper primitive available to us (see §7) |
| Windows | DPAPI / Credential Manager | any process running as the same user | ≈ `chmod 0600` |
| Linux (desktop) | Secret Service via D-Bus | **any process in the user's session** — the API has no per-application ACL | ≈ `chmod 0600`, plus a D-Bus dependency |
| Linux (headless), containers, CI | — | not available at all | — |

So on Linux and Windows the keychain costs a dependency, a daemon, and a class
of prompt failures while delivering the protection of a `0600` file. Only on
macOS does it deliver a property — **code-identity binding** — that no other
mechanism here can replace. That asymmetry is what the new default encodes.

#### What changes

- **Default store, interactive use: `local-vault`.** It is identically
  available on every platform the binary runs on — no D-Bus, no `Security`
  framework, no Credential Manager — and gives one audit surface, one rotation
  flow, and one version-history implementation instead of one per backend.
- **Default store, CI/headless: `env-store`.** Secrets arrive from the CI
  system's own vault as environment variables; there is nothing to unlock. This
  preserves ADR-005's env fallback as the CI answer.
- **The keychain source stays in-tree and is disabled by default**, on every
  platform including macOS. It is enabled by an explicit setting:

  ```toml
  [secrets.keychain]
  enabled = true          # default: false
  ```

  and, once enabled, may be selected as `[default].source` or targeted by an
  individual `[[route]]` in `sources.toml`. No code is deleted; nothing needs a
  community plugin; the user opts in.
- **`Envelope::Keychain` is removed** — but for a reason independent of the
  above, and the two must not be conflated. The variant is **dead code**:
  `Vault::add_keychain_envelope` has no call sites, every production
  `InitialUnlock` sets `with_keychain_account: None`, and the module
  documentation concedes that the wrap key is protected by the standard ACL and
  **not** by Touch ID, so the advertised biometric property was never
  implemented. Unlock methods become `Passphrase` / `Totp` / `Recovery`
  (+ `Keyfile`, below). Enabling `[secrets.keychain]` re-enables the keychain
  as a *store*; it does not resurrect a per-secret biometric envelope that
  never worked.
- **New `Envelope::Keyfile` — opt-in, for unattended cold start.** A 32-byte
  keyfile whose HKDF output wraps `vault_key`, letting a daemon start without a
  human. This is the honest replacement for "the keychain unlocked it for me",
  and it is available on every platform. Two rules make it more than a key left
  beside the lock:
  1. The keyfile defaults **outside** the vault's own directory —
     `<state_dir>/devboy-tools/vault.key`, not `<config_dir>/…/secrets/` —
     so that a backup, cloud sync, or accidental `git add` of the config tree
     does not carry both halves.
  2. It is refused unless its permissions are `0600` and it is owned by the
     invoking user.

  A keyfile does not defend against a same-UID process; neither does the
  keychain on Linux or Windows. It defends against the *file*-level leak, which
  is the realistic accident.
- **CI mode becomes real.** `crates/devboy-storage/src/ci.rs` already defines
  `CiPolicy` with `prefer_env_store: true` and `detect_ci_mode(--ci, DEVBOY_CI,
  [runtime] ci)`, but nothing consumes the policy — today the detection result
  only prints a warning. This ADR gives it a consumer: under CI mode the
  default source is `env-store`, `local-vault` unlock is refused rather than
  prompted, unavailable sources are skipped silently, and each routing decision
  is emitted at info level. The `[runtime] ci` config section referenced by
  that module's documentation is added, since it does not currently exist.
- **ADR-005 is *partially* superseded** — specifically its "keychain as
  primary, env as fallback" decision. Its `SecretString` discipline and its
  env-store fallback remain in force; the frontmatter therefore keeps
  `supersedes: null`, because a partial replacement of one decision is not a
  supersession of the ADR.

#### The CI / env-only mode is a first-class mode, not a degraded fallback

Everything else in this ADR — vault, daemon, unlock windows, TOTP, approvals,
audit log, versioning, §7's process model — assumes a human at a machine. A CI
runner has none of that, and the framework must work there **without any of
it**. This is stated as a hard contract because the failure it prevents is the
worst kind: a pipeline that hangs on an invisible prompt, or that stalls for 25
seconds on a D-Bus call to a Secret Service daemon that does not exist.

**In env-only mode, the *only* secret source is the process environment.**

| Subsystem | Behaviour in env-only mode |
|---|---|
| Secret resolution | environment variables only |
| Local vault | never opened, never created, not required |
| Daemon | never started, never contacted |
| OS keychain | never contacted — no D-Bus, no `Security` framework, no Cred Manager |
| Passphrase / TOTP / keyfile | not applicable; no prompt is ever rendered |
| `approve_on_use` | not applicable — there is no human to approve; a path requiring approval fails closed |
| Audit log (§4), versioning (§5) | not available; the vault they live in is not open |
| §7 trusted path | not applicable; `doctor` reports "env-only", not a trusted-path level |

**Activation.** Three explicit switches, in precedence order — `--ci`, then
`DEVBOY_CI=1`, then `[runtime] ci = true`. Separately, the heuristic variables
(`CI`, `GITLAB_CI`, `GITHUB_ACTIONS`, `BUILDKITE`) **do not silently flip the
mode**; they raise a `doctor` notice telling the user to make it explicit. A
security posture must not change because an unrelated tool exported `CI=1`.

The mode is nevertheless reachable without configuration, because the default
chain after this ADR is env-store first and the keychain is off: a runner that
sets the right variables works out of the box. The explicit switch buys
*strictness* — the guarantees in the table above — not basic functionality.

**Variable naming — both conventions, permanently.** Two incompatible schemes
exist in the tree today, and the default flip must not break pipelines built
against either:

| Origin | Key/path | Variable |
|---|---|---|
| ADR-005 `EnvVarStore` | `github.token` | `DEVBOY_GITHUB_TOKEN`, then unprefixed `GITHUB_TOKEN` |
| ADR-021 `env-store` | `team/gitlab/token-deploy` | `DEVBOY_SECRET__TEAM__GITLAB__TOKEN_DEPLOY` |

Resolution order in env-only mode: (1) the manifest's explicit `env_var` alias
for the path, (2) the ADR-021 convention name, (3) the ADR-005 prefixed name,
(4) the ADR-005 unprefixed name, (5) `DEVBOY_SECRETS_FILE` if set. The
unprefixed fallback is what lets a runner reuse the variables its platform
already exports, and it is retained deliberately rather than deprecated.

**Failure behaviour — fail fast, never prompt, never hang.**

- A missing secret is an immediate error naming **every** variable that was
  looked for, so the fix is copy-pasteable into the CI config.
- No code path may render an interactive prompt. The existing
  `std::io::stdin().is_terminal()` guards become a mode-level invariant rather
  than a per-command precaution.
- No source that can block on IPC (keychain/D-Bus, daemon socket) is consulted
  at all, so there is nothing to time out on.
- A write (`secrets set`, `rotate`, MCP provisioning) returns an explicit
  read-only error. It must **not** silently succeed into a scratch store —
  today `ChainStore::ci_chain()` uses `MemoryStore`, where writes appear to
  work and vanish at process exit. That is acceptable as a test shim and wrong
  as CI behaviour.
- MCP tools that require the vault (`secrets_unlock`, `vault_log_append`,
  version history) return a clear `NotAvailableInCiMode` error rather than
  attempting to start a daemon.

**Container/headless parity.** The same mode covers bare containers and
headless Linux without a Secret Service daemon — the environments that ADR-005
and ADR-021 treated as exceptions requiring a fallback ladder. After this ADR
they run the ordinary path, and it is the *interactive* setup that adds
optional machinery on top.

#### Implementation note: where "the default" actually lives today

This matters for anyone implementing the above, because the ADR-021/023 routing
model is not yet the thing that resolves tokens.

Two stacks coexist. The router (`sources.toml`, `[default].source`,
`PathResolver`) is what ADR-021/023/024 describe — and it currently has **no
runtime consumer**; `RouterConfig::load_from` even returns an empty config when
the file is absent, so the router is opt-in and inert. Every provider token in
the CLI and the MCP server is instead resolved by
`ChainStore::default_chain() = [EnvVarStore, KeychainStore]` in
`crates/devboy-storage/src/lib.rs`, selected in `crates/devboy-cli/src/main.rs`
and gated solely by `DEVBOY_SKIP_KEYCHAIN`.

Flipping `[default].source` therefore changes what `doctor` reports and what the
GUI opens, and **nothing else**. Landing this sub-decision means changing the
chain constructor and its gates as well — and replacing `ci_chain()`'s
`MemoryStore` (writes silently vanish) with the local vault, which is
acceptable as a test shim but not as a default.

#### Threat-model tradeoff, stated openly

Demoting the keychain gives up OS-native protection *by default* on macOS,
where it was real. The vault compensates with Argon2id, AEAD with path-as-AAD,
version history, and §7's process model — but none of those is a secure
element, and none replicates macOS's code-identity binding. That property is
important enough that §7 recommends re-enabling the keychain on macOS
specifically as an anti-tamper measure rather than as a store. On Linux and
Windows nothing of substance is lost, because there was nothing beyond `0600`
to lose.

**Migration.** `devboy secrets migrate` (extended from ADR-020 §8) walks legacy
keychain entries, reads each value once, writes it as the first version of the
corresponding vault path, and removes the keychain entry on explicit user
confirmation. Until a user runs the migration, the legacy keychain reader stays
available regardless of `[secrets.keychain] enabled`; after migration,
`[secrets] migration_complete = true` disables it. Existing users are not
locked out by the default flip.

#### The unattended path, and binding it to a machine (Ф7-2, Ф16)

Without the keychain, nothing opened a vault without a human at a keyboard.
`Envelope::Keyfile` restores that: 32 bytes on disk, outside the config tree,
whose HKDF output wraps the vault key. Enrolment (`devboy secrets keyfile add`)
requires unlocking the vault first — a keyfile is a second door opened from
inside, not a way in — and records the path in configuration, because the daemon
takes it from there and never from a request.

The protection a keyfile offers is that the two halves live in different trees,
so a backup or a sync captures one and not the other. That holds until someone
syncs a whole home directory, copies a container image, or restores a machine
wholesale, at which point both halves travel together and the vault opens
anywhere.

So the derivation also mixes in a machine identifier — `/etc/machine-id`,
`IOPlatformUUID`, `MachineGuid` — and the same two files on a different host
derive a different wrap key. Non-portability is the feature.

What that is worth, stated plainly: nothing against an attacker who is trying,
since every one of those identifiers is readable by any process on the box. It
is worth a lot against the two things in this threat model — accidental
disclosure (a synced directory, a shared backup, an image in a registry) and
generic credential harvesters, which collect files by shape and do not
reconstruct a per-host derivation.

Three details keep it from becoming a support burden:

- **The binding is recorded in the envelope, not inferred.** An environment with
  no stable identifier gets an unbound envelope rather than a failure, and
  envelopes written before this existed keep opening.
- **A missing identifier is an error, never a silent fallback to unbound.**
  Falling back would turn "this machine changed" into a decryption failure
  indistinguishable from a wrong keyfile.
- **The recovery is cheap and named in the error text**: enrol again on this
  machine. Combined with short-lived tokens, losing a machine-bound vault costs
  a re-onboarding, not a recovery operation.

### 7. Trusted path — the process model that makes §1–§6 mean anything

#### The problem

ADR-023 describes the unlock modal as "agent-bypassing", but nothing in the
implementation makes it so. The daemon, the CLI, and the agent all run under
the **same UID**, and the passphrase is collected by `dialoguer::Password`
*inside the CLI process* (`crates/devboy-cli/src/secrets_cmd.rs`) — the very
process an agent with shell access can replace on `PATH`, wrap via
`LD_PRELOAD`, or shadow through a shell rc file.

The consequence is uncomfortable and worth stating plainly: **no choice of
unlock method fixes this.** Passphrase, keyfile, TOTP, hardware token — all are
equally exposed when the process collecting them is untrusted. §1's careful
placement of `totp_secret` beyond the agent's reach is worth nothing if the
agent simply harvests the passphrase that unlocks the vault in the first place.

This is a **process-model** problem, and it needs a process-model answer.

#### The principle

> Anything requiring the user's trust — collecting the passphrase, approving a
> secret access, displaying *what* is being requested — must happen in a
> process the agent can neither read nor substitute.

Three concentric levels, in decreasing order of strength. An implementation
should reach for the strongest available and degrade explicitly, reporting the
achieved level through `doctor` rather than silently.

**Level 1 — separate UID (target).** The daemon runs under its own service
account; its binary is not writable by the user, so the agent cannot replace
it. It collects the passphrase itself and renders approval prompts itself; the
CLI only signals "an unlock is needed". A substituted CLI can then request
operations but can never observe the passphrase, and `ptrace` across UIDs is
denied outright.

  Note for implementers: the socket currently authenticates peers by
  `peer_uid == geteuid()` (`crates/devboy-secrets-agent/src/socket.rs`). Under
  a split UID that predicate becomes wrong and must be replaced by an explicit
  allow-list of client UIDs; keeping the equality check would either lock out
  the legitimate user or, if "fixed" by widening it, authenticate everyone.

  **File ownership follows the daemon, not the user.** A separate UID is not
  merely a socket change: the daemon must be able to read the vault, and the
  user must *not* be able to read it directly, or the split buys nothing. The
  vault file, the audit log, and any keyfile (§6) therefore move to storage
  owned by the service account — `/var/lib/devboy-secrets/<user>/` or an
  equivalent per-user subdirectory with mode `0700` — rather than living under
  `$HOME` where the agent can read them. This is the largest practical cost of
  level 1 and the reason level 2 exists: it changes install layout, requires
  privileged setup, and needs a migration for existing vaults.

  **Multi-user hosts** run one daemon instance per user account under distinct
  service UIDs, or a single daemon that partitions vaults by requesting UID and
  never serves one user's paths to another. The former is simpler and is
  recommended; the latter concentrates every user's `vault_key` in one process
  and is not.

**Level 2 — same UID, independent lifecycle (fallback).** Where a service
account cannot be created (unprivileged install, per-user setup), the daemon
runs as the user but is started by systemd user unit / launchd / login shell —
**never lazily spawned by the agent**. Under the common
`kernel.yama.ptrace_scope = 1` policy a process may only `ptrace` its
descendants, so a daemon that is not the agent's child keeps its memory —
including `totp_secret` and `vault_key` — out of the agent's reach.

  This makes an operational rule normative: **a lazily-started daemon whose
  parent is the agent must not be trusted for the §1 TOTP path**, and should
  refuse to serve `secrets_unlock`. Otherwise §1's guarantee silently
  evaporates on exactly the machines that need it.

**Level 3 — same UID, agent-parented (degraded).** No meaningful protection
against a hostile agent; acceptable only for a trusted single-user workstation.
`doctor` must say so rather than imply the guarantees of levels 1–2.

#### Self-check: the daemon verifies its own provenance

"The agent must not start the daemon" is an operational rule, and operational
rules that nobody enforces decay into comments — the same reasoning that made
§4's scrub server-side and §5's purge user-only. The daemon can check this
about **itself**, so it must.

Three checks, none of which requires knowing anything about which agent is
running:

- **A. Ancestry, on every connection.** The daemon reads the client's PID from
  the socket (`SO_PEERCRED` on Linux, `LOCAL_PEERPID` on macOS,
  `GetNamedPipeClientProcessId` on Windows) and walks its **own** parent chain.
  If the connecting client appears in it, the client is an ancestor of the
  daemon — and can therefore `ptrace` it under `ptrace_scope = 1`. Every
  guarantee that depends on daemon memory being private is void for that
  client.
- **B. Startup provenance, once at launch.** A daemon started the intended way
  is reparented to the init system: PID 1, or `systemd --user`, or `launchd`,
  or the Windows service manager. A daemon whose parent is an ordinary session
  process was spawned from inside someone's process tree, which is the
  condition this rule exists to prevent.
- **C. No controlling terminal.** A correctly launched daemon has none. Holding
  one indicates it was started from a shell, and — combined with §7's PTY
  requirement — that its prompts may render into a terminal someone else owns.

**This is deliberately structural, not nominal.** The daemon never asks "is my
ancestor a coding agent" — it could not answer that without embedding a list of
vendor process names, which CI guard #243 forbids and which would fail on the
first agent not on the list. It asks "can my caller `ptrace` me", which is the
property that actually matters and is the same question regardless of what the
caller is.

**What happens on failure: fail closed.** A degraded-but-running daemon is the
outcome this ADR spends §7 arguing against — it preserves the appearance of a
guarantee that no longer holds, and a user who does not read status output will
never learn the difference. The default is therefore to stop, loudly.

| Check | Severity | Default behaviour |
|---|---|---|
| **A** — client is an ancestor of the daemon | **fatal** | Send `DaemonUntrusted` with its §8 remediation, then close the connection. The client is refused entirely, not merely denied the TOTP path |
| **B** — parent is not the init system | **fatal** | Refuse to start; exit non-zero with the platform's correct launch command in the message |
| **C** — controlling terminal present | warning | Start, but warn on every launch and report it in `secrets_status()` and `doctor` |

C is a warning rather than fatal because holding a TTY is not itself a
`ptrace` capability — it is a strong hint about *how* the daemon was launched,
and about §7's prompt-channel requirement, but the actual exposure is what A
and B measure.

**The escape hatch, and its honest weakness.** Integration tests spawn the
daemon from a test harness, and a developer debugging locally starts it from a
shell; both fail check B legitimately. Setting
`DEVBOY_INSECURE_ALLOW_UNTRUSTED_DAEMON=1` downgrades A and B from fatal to
warning.

The name carries `INSECURE` deliberately, so that its appearance in a CI file,
a Dockerfile, or a diff is self-documenting. When set:

- a warning is emitted on **every** daemon launch and **every** affected
  connection — never once at startup, because a single line scrolls away;
- `trust_level` still reports the real value (`agent_parented`), because the
  override changes what is *permitted*, never what is *claimed*;
- `available_methods` still omits `"totp"` — the override does not resurrect a
  guarantee the process layout cannot support;
- `doctor` reports it as a finding, not as configuration;
- the audit log (§4) records it, when a vault is open.

Its weakness must be stated rather than discovered: an agent that starts the
daemon already controls the daemon's environment and can set this variable
itself. The override therefore protects against **accidental** misconfiguration
and against an agent acting in good faith — the threat model this ADR claims —
and not against a hostile one, which §7's limits already concede. Its value is
that the insecure path cannot be reached *silently* or by default.

**Agent-facing consequence (§8).** `DaemonUntrusted` carries
`actor: "user"`, `action: "ask_user_to_restart_daemon"`, and a
`user_message` naming the platform command — for example *"The secret daemon
was started by this session and cannot protect its own memory from it. Stop it
and start it via `systemctl --user start devboy-secrets`, then retry."*

Critically, this reply is **sent before the connection closes**, not instead of
a response. A refused client that receives a dropped socket learns only that
something broke and will retry or improvise; a client that receives
`DaemonUntrusted` with its remediation knows to stop, tell the user exactly
what to run, and wait. The whole point of §8 is that a hard failure still
arrives as an instruction. The agent must not restart the daemon itself, since
doing so reproduces exactly the condition being reported.

**Limits, stated so the check is not oversold.**

Check A is defeated by double-forking: an agent that starts the daemon and lets
it reparent to init breaks the ancestry chain and passes. But that manoeuvre is
self-defeating for the attack it would enable — `ptrace_scope = 1` evaluates
descent *at the time of the `ptrace` call*, so an orphaned daemon is no longer
the agent's descendant and is no longer `ptrace`-able by it. The escape from
the check is also an escape from the capability.

What none of these checks catch is an agent that controlled the daemon's
**environment** at launch — `LD_PRELOAD`, a modified `PATH`, a substituted
binary — regardless of the resulting process tree. That is a code-integrity
problem, not a topology problem, and its answer is level 1 (a daemon binary the
user cannot write) or the macOS code-signature binding described below. The
self-check raises the floor; it does not replace §7's levels.

#### Two credential flows, and the rule that separates them

The design has exactly two paths by which a credential reaches the daemon, and
they are deliberately different shapes:

```
passphrase   user ──────────────────────────────────▶ daemon
             (§7 trusted path; the agent is not in this path at all,
              and has no tool that would accept a passphrase)

TOTP code    user ──▶ agent ──▶ MCP ──▶ daemon
             (the agent is a transport; it sees the code and cannot
              do anything useful with it)
```

The asymmetry follows from one rule, which is worth stating because it decides
any future credential too:

> A credential may cross the agent surface **only if** it is ephemeral **and**
> cannot perform a cold start.

| | passphrase | TOTP code |
|---|---|---|
| Lifetime | permanent | one 30 s step, then spent |
| What it opens | the vault from nothing | a session a human already opened this boot |
| Can the agent derive it? | — | no (§1: secret is vault-resident and daemon-held) |
| If it lands in a transcript | compromised forever | worthless |

Both conditions are required. A TOTP code stored where the agent could read its
secret would fail the second test even while passing the first, which is
exactly the mistake the original §1 made.

**Consequence for the passphrase prompt: it must not render into a terminal the
agent controls.** If the agent spawned the shell, it owns the PTY master and
can read everything typed into it — including input that is not echoed. A
daemon-rendered prompt is only trustworthy on a channel the agent does not
hold: its own GUI/TUI window, a separate terminal the user opened, or a
platform primitive from the table below. "The daemon printed the prompt" is not
sufficient; *where* it printed is the property that matters.

**How the agent learns the unlock happened.** It does not observe the
passphrase flow, so it polls `secrets_status()` or simply retries the failed
operation after telling the user. There is deliberately no completion callback
from the passphrase path to the agent — the agent is not a participant in it.

**A rule to state in user-facing documentation:** *devboy never asks for your
vault passphrase in an agent conversation.* No tool accepts one, so any chat
message requesting it is either a confused agent or a hostile prompt, and the
answer is always no. This is the secret framework's equivalent of "your bank
will never ask for your PIN", and it is the user's half of the §7 contract —
the framework can guarantee it never asks, but only the user can refuse to
answer. A passphrase typed into a chat window has entered the transcript and
should be treated as compromised: rotate it via `devboy secrets vault
change-passphrase`.

**Residual risk, stated openly.** Because the agent renders the TOTP request in
its own conversation, it can ask for a code when nothing needs unlocking and
obtain a premature unlock. §8 requires it to display the daemon-authored
`user_message`, but nothing forces it to. The blast radius is bounded: the
agent gains an unlock window it could have obtained anyway by waiting for the
user's next legitimate unlock, and it still never receives `vault_key` or any
secret value. Users who consider this unacceptable should use the `strict`
profile of §2, where per-call approval makes each subsequent access a separate
human decision.

#### The conflict inside this section, and how it is resolved (Ф14)

§7 asks for two things that pull against each other. The daemon must be
reparented to init, so that no process the agent controls is its parent. And
the daemon must collect the passphrase itself, so that nothing the agent
controls sees it typed.

A reparented process has no controlling terminal. Our own systemd unit sets
`StandardInput=null`; launchd is no different. So the daemon that satisfies the
first requirement has no screen on which to satisfy the second. For a while
that was simply the state of things: `vault.request_unlock` answered "no prompt
surface", and the only way into a locked vault was
`DEVBOY_VAULT_PASSPHRASE` — adequate for a server, useless for a person at a
laptop. Interactive unlock did not work in the configuration this ADR
recommends.

**Resolution: the caller lends a terminal.** The client has one, because a
human just typed a command into it. It resolves that terminal to a concrete
path — `/dev/pts/3`, never the per-process `/dev/tty` — and names it in the
request. The daemon opens that path and asks there. The passphrase still never
crosses the socket and never enters the client's memory; only the *location of
the screen* comes from the caller.

**Why that does not give the game away.** The objection is obvious: §7 exists so
the prompt lives on a channel the agent does not own, and here the caller picks
the channel. Worked through, nothing is lost. An agent that names a terminal it
controls gains nothing, because nobody types into it — the passphrase comes
from a human looking at their own screen. Guessing is no better: `vault.unlock`
already accepts a passphrase outright, so that oracle always existed. And an
agent that wants to trick a human into typing a passphrase where it can read it
never needed any of this; it can print its own prompt.

What the daemon rests on is *provenance* — who started it, and whether the
caller is an ancestor that could read its memory. Neither is affected by which
terminal is named. The path decides where the question is printed, not whether
the answer can be trusted. The audit entry records which channel was used
(`channel=own` or `channel=client`), because those are different enough that a
reader of the trail should not have to guess.

**What is refused.** The daemon opens a caller-supplied path read-write, so the
path must be under `/dev` (checked before opening — otherwise the prompt text
would land in whatever file was named) and the result must be a terminal
(checked after opening, since only the descriptor can answer that). A pipe
would mean a script is answering, and the whole arrangement is built on a human
having been present.

**Mechanism note.** Passing the descriptor itself over the socket
(`SCM_RIGHTS`) was the original plan and is the more obvious design. Adopting a
received descriptor requires `OwnedFd::from_raw_fd`, which is `unsafe`, and
this workspace sets `unsafe_code = "forbid"` — no local exception is possible,
and the fd-passing crates return raw descriptors too, so each would only move
the same `unsafe` somewhere less visible. Naming the terminal reaches the same
place with an ordinary `File::open`. The one real difference: a daemon in a
different mount namespace from its client (a container) may not have that path,
where `SCM_RIGHTS` would still work. That is the reason to revisit this if
namespaces ever come up.

#### Platform trusted-path primitives

Where the OS offers a real trusted path, use it in addition to the levels above:

| Platform | Primitive | What it gives |
|---|---|---|
| **Windows** | `CredUIPromptForWindowsCredentials` with `CREDUIWIN_SECURE_PROMPT` (secure desktop); SAS handled by the kernel | A genuine trusted path — input cannot be observed or spoofed by a user-mode process |
| **macOS** | `LocalAuthentication` (`LAContext.evaluatePolicy`); Authorization Services rendering in `SecurityAgent`, a separate process under its own UID | Input never reaches the calling process. No SAS equivalent, so a *visual* imitation remains possible — but the imitation cannot obtain the real result |
| **Linux** | None for user-session applications. polkit governs system actions; PAM runs inside the calling process; `systemd-ask-password` is system-scoped | Fall back to level 1/2. **Wayland** matters here: it has no global input capture, whereas **X11 permits any client to grab the keyboard**, making passphrase entry indefensible on X11 |

**macOS keychain, re-entering as anti-tamper.** §6 disables the keychain as a
store by default, and on Linux/Windows that costs nothing. On macOS, however,
keychain ACLs are bound to the **code signature** of the reading process — the
only mechanism in this entire design that distinguishes "the real `devboy`"
from "a replaced binary at the same path". A substituted binary does not
inherit access; the system prompts instead.

Users on macOS who want defence against binary substitution should therefore
enable `[secrets.keychain]` and store the vault's keyfile or an unlock-gating
token there — not as a secret store, but as a **code-identity check**. This is
a genuinely different role from the one §6 demotes, and the configuration
surface should name it as such rather than reusing "keychain = my secrets live
here".

#### The honest limit

Everything above protects the **credential**. None of it protects the
**authorization**:

> An agent that has achieved persistence can simply wait for the user to unlock
> legitimately and act inside the open window.

No prompt, no secure desktop, no hardware token prevents this — the human
approved a real unlock, and the agent is a valid client of the resulting
session. The only mitigations are shrinking the window and requiring a fresh
human decision per access, which is precisely the `strict` profile of §2
(`unlock_ttl = 15 min`, `approve_on_use` forced to `per-call`, prompts rendered
by the daemon). That is a real cost in friction, which is why it is a profile
and not the default.

The complete answer lies outside this repository: run the agent under a
**different UID, namespace, or container without access to the user's home
directory**, so that the file-level boundary becomes real rather than
notional. This ADR cannot impose that — it is a property of how the agent is
launched — but the process model here is designed so that such a deployment
strengthens it automatically rather than requiring a redesign.

### 8. Actionable errors — the agent always knows what to do next, including when to fetch a human

#### The problem

Every error in §1–§7 tells the agent *what failed* and nothing about *what to
do about it*. An agent that receives `{ error: "Locked" }` has to guess, and
each plausible guess is bad:

- ask the user for the **passphrase** — forbidden by §3, and it trains users to
  type their master credential into a chat window;
- **start the daemon itself** — which makes the daemon its own child and
  silently voids §1 and §7 level 2, converting a security guarantee into its
  appearance;
- **look for the secret elsewhere** — environment, dotfiles, git history — and
  drag a value into its context in direct violation of ADR-023's invariant;
- **retry in a loop** — burning the §1 rate limiter and locking the user out;
- **give up** with "secrets are not working", leaving the user to diagnose a
  framework they cannot see into.

The framework knows exactly which of these is correct in every case. It should
say so, in a machine-readable form, rather than leaving a language model to
infer it from an error name.

#### Shape

Every `secrets_*` / `vault_*` error reply carries a `remediation` object:

```
{
  error: "Locked",
  remediation: {
    actor:        "agent" | "user",   // who can actually resolve this
    action:       "request_totp",     // machine-readable next step
    user_message: "The secret vault is locked. Enter the 6-digit code from
                   your authenticator app to unlock it.",
    retryable:    true,
    retry_after_seconds: null         // set for rate limits / backoff
  }
}
```

`actor` is the load-bearing field: it tells the agent whether this is its
problem or whether it must stop and fetch a human. `action` exists so the agent
branches on a constant rather than parsing prose. `user_message` is **composed
by the daemon**, not by the agent, and is meant to be surfaced to the user
verbatim.

#### Error → remediation contract

| Error | `actor` | `action` | Meaning for the agent |
|---|---|---|---|
| `Locked`, TOTP session resident | agent | `request_totp` | Ask the user for a code, relay via `secrets_unlock`, retry |
| `Locked` / `TotpUnavailable`, no session | **user** | `ask_user_to_unlock` | Stop. Only a passphrase at the §7 prompt opens this |
| `BadTotp` | agent | `request_totp` | Code was wrong — ask once more, then escalate |
| `ReplayedCode` | agent | `request_fresh_totp` | Code already spent; wait for the next 30 s step |
| `RateLimited` | agent | `retry_after` | Back off exactly `retry_after_seconds`; do not loop |
| `NotProvisioned` | **user** | `ask_user_to_provision` | Secret does not exist. Surface `retrieval_url` + `required_scopes` |
| `ApprovalRequired` | **user** | `ask_user_to_approve` | A prompt is waiting on the daemon's surface; wait, do not retry |
| `ApprovalDenied` | **user** | `none` | The user said no. Do not re-ask in this session |
| `LivenessFailed` / expired | **user** | `ask_user_to_rotate` | Token is dead. Surface `retrieval_url` + `rotation_method` |
| `DaemonNotRunning` | **user** | `ask_user_to_start_daemon` | **Never start it yourself** — see below |
| `DaemonUntrusted` | **user** | `ask_user_to_restart_daemon` | The daemon found you in its own ancestry (§7 check A) and is closing the connection. Relay the restart command; restarting it yourself reproduces the fault |
| `NotAvailableInCiMode` | **user** | `set_env_var` | Name every variable that would satisfy this path (§6) |

#### The manifest already holds the useful part

`IndexEntry` (`crates/devboy-storage/src/index.rs`) carries `description`,
`retrieval_url`, `required_scopes`, `rotation_method` and `expires_at`. These
exist to be shown to a human at exactly this moment, and today nothing shows
them. A `NotProvisioned` remediation should read

> "GitLab deploy token (`team/gitlab/token-deploy`) is not set up. Create one
> with scopes `api`, `read_repository` at https://gitlab.example/-/user_settings
> then run `devboy secrets set team/gitlab/token-deploy`."

rather than "secret not found". None of those fields is a secret value, so the
whole `remediation` object satisfies `AgentSafeReply`'s audit checklist
unchanged — it is metadata plus fixed text, and it must be added to the
compile-time reply fence like any other reply type.

#### Negative contract — what the agent must never do

Stated here because these are the failure modes that silently dismantle the
other sub-decisions, and an agent cannot infer them:

1. **Never request the passphrase.** No tool accepts one. An agent asking for
   it in chat is out of protocol regardless of how the request is phrased.
2. **Never start or restart the daemon.** `DaemonNotRunning` and
   `DaemonUntrusted` are `user` actions specifically because a daemon spawned
   by the agent is a daemon the agent can `ptrace` (§7). The remediation names
   the platform command for the user to run; the agent relays it and waits.
   This is not an honour-system rule — §7's check A detects the resulting
   layout and refuses the TOTP path for that client — but an agent that
   "helpfully" restarts the daemon converts a clear error into a silently
   degraded session, so the prohibition is stated as well as enforced.
3. **Never work around a missing secret.** Reading it from the environment,
   a dotfile, a config sample, or shell history defeats ADR-023's boundary just
   as thoroughly as leaking it would.
4. **Never retry past `retryable: false`,** and never faster than
   `retry_after_seconds`.

#### Prompt-injection posture

`user_message` is generated by the daemon from the error kind plus manifest
metadata. The agent does not compose it and cannot influence its content, which
closes the injection concern already noted in the Risks section: hostile text
in a repository cannot cause a *misleading* unlock request, because the wording
never originates agent-side. The user still sees a fixed, framework-authored
sentence, and what they type back is a 6-digit number with no room to carry an
instruction.

### Positive

- ✅ **Mid-session re-lock stops being fatal.** An agent running for hours can
  ask the user for a TOTP in-band and continue, without a GUI modal or leaving
  the terminal.
- ✅ **TOTP now carries a property it can actually deliver.** With
  `totp_secret` vault-resident and daemon-held, a valid code is evidence of
  human presence that the agent cannot fabricate — instead of a shorter
  passphrase whose strength silently collapsed to that of its keystore.
- ✅ **The unlock stack no longer depends on any OS keystore.** Passphrase
  works everywhere; `Envelope::Keyfile` covers unattended cold start; TOTP
  covers in-session re-unlock. CI, containers, and headless Linux stop being
  fallback paths and become ordinary ones.
- ✅ **Daily friction stays low.** `unlock_ttl` defaulting to a working day
  removes the 15-minute re-lock churn while keeping an explicit ceiling, and
  the `strict` profile is one setting away when the posture matters more.
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
- ✅ **One store, every platform.** Demoting the keychain removes the
  CI/headless/corporate-Mac fallback ladder and leaves a single cross-platform
  encrypted vault as the default store — while keeping the keychain one
  setting away for those who want it.
- ✅ **The trust boundary is now stated in terms of processes, not intentions.**
  "Agent-bypassing" stops being an aspiration in prose and becomes a
  requirement on where the daemon runs and who renders a prompt — testable,
  and reportable by `doctor`.
- ✅ **The agent never has to guess, and knows when to fetch a human.** Every
  failure names who can fix it and what the next step is, so "vault locked"
  becomes a concrete request for a code or a concrete request for the user —
  instead of a language model improvising around a security boundary.
- ✅ **The manifest metadata finally reaches the person who needs it.**
  `retrieval_url`, `required_scopes` and `rotation_method` have existed since
  ADR-020 with nothing surfacing them; a `NotProvisioned` error now tells the
  user which token to create, with which scopes, and where.

### Negative

- ❌ **Two more unlock methods to maintain.** TOTP enrollment, QR rendering,
  drift handling, replay tracking and rate-limiting, plus the keyfile envelope
  and its permission checks, are new surface in `devboy-vault-crypto` and the
  daemon.
- ❌ **A longer unlock window widens the in-memory exposure of `vault_key`.**
  Stated openly; bounded by `max_unlock_ttl`; the user's explicit choice.
- ❌ **Agent-mediated unlock is a trust-boundary exception.** Even though the
  relayed credential is one the agent cannot derive, it is one more thing to
  document and one more `secrets_*` tool whose contract must be auditable.
- ❌ **A new on-disk file** (`audit-log.dvb`) and a new `vault_log_*` tool
  family.
- ❌ **Loss of macOS code-identity binding by default.** Demoting the keychain
  gives up the ACL-by-code-signature property on the one platform that offers
  it. §7 recommends re-enabling it there specifically as anti-tamper, but that
  is now an opt-in the user must know to make.
- ❌ **§7 has real operational cost.** A separate service UID means packaging
  work, a systemd/launchd unit, and a socket authorization model that is no
  longer a simple UID equality check. Level 2 is cheaper but weaker, and the
  difference must be surfaced honestly rather than assumed.
- ❌ **Every error reply grows a `remediation` object.** More surface to keep
  correct and to audit against `AgentSafeReply`, and a wrong `actor` is worse
  than no hint — it sends the agent looping on something only a human can fix,
  or the reverse. The mapping in §8 has to be maintained as errors are added.
- ❌ **Two profiles instead of one default.** Users now face a choice
  (`convenient` / `strict`) where previously there was a single number. This
  is deliberate — the two goals genuinely conflict — but it is added surface
  in the configuration and in the documentation.

### Risks

- ⚠️ **TOTP replay within the 30 s window.** An attacker with read access to
  the transcript *and* local socket access *and* acting within 30 s could
  replay the code. **Mitigation:** the replay guard rejects an already-spent
  time-step outright (§1), plus daemon rate-limiting and the socket UID check.
- ⚠️ **`DEVBOY_INSECURE_ALLOW_UNTRUSTED_DAEMON` becoming ambient.** The escape
  hatch that unblocks tests and local debugging is an environment variable, and
  an agent that starts the daemon controls its environment — so a hostile agent
  can set it. It can also drift into a Dockerfile or CI template and stay there.
  **Mitigation:** `INSECURE` in the name makes it self-documenting in review;
  the warning repeats on every launch and every affected connection rather than
  once; `trust_level` and `available_methods` keep reporting the real posture,
  so the override changes what is permitted and never what is claimed; `doctor`
  reports it as a finding. The residual exposure is the one §7 already concedes
  — a hostile agent that controls the launch environment is out of scope.
- ⚠️ **Fail-closed checks blocking a legitimate setup.** Check B fails for any
  daemon a developer starts by hand, which is a normal debugging workflow, and
  a hard failure there is a support burden. **Mitigation:** the error message
  carries the correct platform launch command, and the documented override
  exists precisely for this case; check C stays a warning because holding a TTY
  is a hint about launch method, not a `ptrace` capability.
- ⚠️ **A "daemon-rendered" prompt inside the agent's own terminal.** Moving
  passphrase collection into the daemon achieves nothing if the prompt is
  written to a PTY whose master the agent holds — it reads non-echoed input
  there just as easily. **Mitigation:** §7 requires the prompt to render on a
  channel the agent does not control (the daemon's own window, a separate
  user-opened terminal, or a platform primitive); `doctor` should treat "prompt
  channel owned by a descendant of the agent" as level 3, not level 2.
- ⚠️ **The agent soliciting a TOTP code when nothing needs unlocking.** It
  renders the request in its own conversation and can ignore the
  daemon-authored `user_message`. **Mitigation:** bounded blast radius — a
  premature unlock is one the agent could obtain by waiting anyway, and it
  yields neither `vault_key` nor any value; the `strict` profile makes each
  subsequent access a separate human decision.
- ⚠️ **A user typing the passphrase into the chat window.** No tool accepts
  one, so it cannot reach the daemon — but it has entered the transcript.
  **Mitigation:** the documented rule that devboy never asks for the passphrase
  in an agent conversation, plus a `change-passphrase` path for when it happens
  anyway.
- ⚠️ **A daemon spawned by the agent silently voids §1.** If the daemon is
  lazily started as a child of the agent, `ptrace_scope = 1` no longer
  separates them and `totp_secret` becomes readable — while every guarantee in
  this ADR still *appears* to hold. **Mitigation:** §7 makes independent
  startup normative, requires such a daemon to refuse `secrets_unlock`, and
  requires `doctor` to report the achieved trusted-path level. This is the
  most likely way to implement the ADR correctly on paper and wrongly in
  practice.
- ⚠️ **Keyfile and vault leaking together.** A backup or sync that captures
  both halves reduces the keyfile envelope to no protection at all.
  **Mitigation:** the keyfile defaults outside the config tree (§6) and
  requires `0600` ownership; the documentation must state plainly that a
  keyfile guards against file-level leaks, not against a same-UID process.
- ⚠️ **The two env-variable conventions diverging.** ADR-005's `EnvVarStore`
  reads `DEVBOY_GITHUB_TOKEN` / `GITHUB_TOKEN`; ADR-021's `env-store` reads
  `DEVBOY_SECRET__TEAM__GITLAB__TOKEN_DEPLOY`. Routing CI through only the
  latter would break every pipeline written against the former — silently, as
  a missing secret rather than an obvious error. **Mitigation:** §6 pins the
  five-step resolution order across both conventions as a contract, keeps the
  unprefixed fallback, and requires the not-found error to list every variable
  that was tried.
- ⚠️ **CI writes vanishing instead of failing.** `ChainStore::ci_chain()`
  currently pairs the env store with `MemoryStore`, so a write in CI appears to
  succeed and is lost at process exit. **Mitigation:** env-only mode returns an
  explicit read-only error on writes; `MemoryStore` stays a test shim.
- ⚠️ **The default flip stranding existing users.** Someone whose tokens live
  in the OS keychain today would, after upgrading, find them unresolvable.
  **Mitigation:** the legacy keychain reader stays active until
  `[secrets] migration_complete = true`, independently of the new
  `[secrets.keychain] enabled` switch, and `doctor` should point at
  `devboy secrets migrate` when it sees legacy entries.
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
who wants strictly agent-bypassing unlock does not enroll TOTP and selects the
`strict` profile of §2, where the short window and per-call approval reinstate
exactly this alternative's posture).

### Alternative 2: Passphrase relay instead of TOTP

**Description:** Let the agent relay the master passphrase to a `secrets_unlock(passphrase)`.

**Why rejected:** Two independent reasons, either sufficient. The passphrase is
*persistent* — a transcript containing it is compromised forever, whereas a
spent TOTP step is worthless. And the passphrase is the **cold-start** key: it
unlocks the vault from nothing, so handing it to the agent hands over the
entire store, while a TOTP code only re-opens a session a human already
established this boot. A persistent, cold-start-capable credential must not
cross the agent surface under any framing.

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

### Alternative 6: TOTP as a replacement for the passphrase ("six digits in the morning")

**Description:** Enroll TOTP, store `totp_secret` in the OS keystore, and let
the user unlock the vault each morning by typing six digits instead of a long
passphrase. This was the original §1 of this ADR.

**Why rejected:** The strength of such an unlock equals the strength of
wherever `totp_secret` is stored, never the ~20 bits of the code — a code is
always derivable from its secret. On Linux the Secret Service hands stored
secrets to any process in the user's session with no per-application ACL, so
the scheme reduces to `chmod 0600` while presenting itself as a second factor.
Worse, it made §1 depend on the very keystore §6 was demoting, so the two
sub-decisions contradicted each other. Repositioning TOTP as an in-session
re-unlock with a vault-resident secret gives it a property it can actually
deliver (evidence of human presence) and removes the keystore dependency
entirely.

### Alternative 7: Remove the in-tree keychain source outright

**Description:** Delete `crates/plugins/secrets/keychain/`; users who want OS
keychain backing install a community source plugin.

**Why rejected:** Two reasons. First, §7 identifies a property that *only* the
macOS keychain provides — ACLs bound to code signature, the sole defence
against binary substitution available in this design — so removing it discards
the one mechanism worth keeping. Second, removal and default-off achieve the
same practical outcome (nobody depends on the keychain by default) while
removal additionally costs every macOS user a plugin install and costs the
project a supported code path. A default flip plus `[secrets.keychain]
enabled` is strictly less destructive and equally decisive.

### Alternative 8: Keep the daemon under the user's UID and rely on the documented "agent-bypassing" contract

**Description:** Leave the process model as ADR-023 describes it, and treat the
unlock modal as agent-bypassing by convention.

**Why rejected:** It already is not. The passphrase is collected inside the CLI
process, which an agent with shell access can replace, and the daemon shares
the agent's UID. A contract that the implementation does not enforce is a
comment, not a boundary — the same reasoning that made §4's scrub server-side
and §5's purge user-only. §7 states what has to be true of the processes and
lets `doctor` report which level was actually achieved, rather than asserting a
guarantee the deployment may not have.

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
  - `crates/devboy-vault-crypto/src/format.rs` — add `Envelope::Totp` and
    `Envelope::Keyfile`, remove `Envelope::Keychain`; TOTP verify + envelope
    wrap. New dependencies are required for RFC 6238: an HMAC-SHA1
    implementation (`hmac` + `sha1`), a base32 codec for `otpauth://` URIs
    (`data-encoding`), and a constant-time comparison (`subtle`) — none of
    which are in the workspace today.
  - `crates/devboy-secrets-agent/` — hold `totp_secret` in daemon memory
    (zeroized on drop) after a passphrase unlock, read from the reserved
    `__totp/secret` slot; enforce the replay guard on accepted time-steps.
  - `crates/devboy-secrets-agent/src/idle.rs` — generalize
    `DEFAULT_IDLE_TIMEOUT` / `IdleTracker` to a configurable window
    (`unlock_ttl`, `max_unlock_ttl`, optional `idle_relock`), driven by the
    `convenient` / `strict` profiles; add TOTP verification + rate-limit to the
    unlock path.
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
  - `crates/plugins/secrets/keychain/` — **keep**, but gate behind
    `[secrets.keychain] enabled` (default `false`). `local-vault` becomes the
    `[default]` source; `env-store` becomes the CI default. `devboy secrets
    migrate` — extend to read each legacy keychain entry once and write it as a
    vault version, with `[secrets] migration_complete` disabling the legacy
    reader afterward.
  - `crates/devboy-storage/src/lib.rs` + `crates/devboy-cli/src/main.rs` —
    **this is where the default actually lives.** Change
    `ChainStore::default_chain()` off `[EnvVarStore, KeychainStore]`, replace
    `ci_chain()`'s `MemoryStore` with the local vault, and widen the
    `DEVBOY_SKIP_KEYCHAIN` gates in `get_credential_store` / `build_mcp_store`
    to honour CI detection and the new setting. Editing `sources.toml` alone
    changes nothing at runtime (see §6's implementation note).
  - `crates/devboy-storage/src/ci.rs` — give `CiPolicy` a consumer; today
    `detect_ci_mode` is called once and its result only prints a warning.
    Env-only mode must additionally: refuse writes with an explicit read-only
    error instead of routing them to `MemoryStore`, never consult a
    blocking-IPC source, and produce a not-found error listing every variable
    tried across both naming conventions.
  - `crates/plugins/secrets/env-store/src/lib.rs` +
    `crates/devboy-storage/src/lib.rs` — implement the five-step resolution
    order of §6 so the ADR-005 names (`DEVBOY_GITHUB_TOKEN`, unprefixed
    `GITHUB_TOKEN`) keep resolving alongside the ADR-021 convention name.
    Regression tests must cover both, or the default flip breaks pipelines
    silently.
  - `crates/devboy-mcp/src/secrets_tool.rs` — vault-dependent tools
    (`secrets_unlock`, `vault_log_append`, version history) return
    `NotAvailableInCiMode` rather than trying to start a daemon.
  - `crates/devboy-core/src/config.rs` — extend `SecretsConfig` (currently a
    single `migration_complete` field) with `profile`, `unlock_ttl`,
    `max_unlock_ttl`, `idle_relock`, `[secrets.keychain] enabled`, and the
    keyfile path; add the `[runtime] ci` section that `ci.rs` documents but
    which does not exist.
  - `crates/devboy-secrets-agent/` + packaging — §7 process model: a
    daemon-rendered passphrase prompt (moving collection out of
    `crates/devboy-cli/src/secrets_cmd.rs`), systemd user unit / launchd
    plist for independent startup, a prompt channel the agent does not own (its
    own window / a separate terminal / a platform primitive — **not** a PTY
    whose master a descendant of the agent holds), refusal to serve
    `secrets_unlock` for a client found in the daemon's own ancestry, and — for
    level 1 — a service UID
    with a client-UID allow-list replacing the `peer_uid == geteuid()` check in
    `crates/devboy-secrets-agent/src/socket.rs`.
  - `crates/devboy-mcp/src/` — a `Remediation { actor, action, user_message,
    retryable, retry_after_seconds }` type attached to every `secrets_*` /
    `vault_*` error reply, composed daemon-side from the error kind plus
    `IndexEntry` metadata (`retrieval_url`, `required_scopes`,
    `rotation_method`, `description`). It must implement `AgentSafeReply` and
    be added to the compile-time reply fence in
    `crates/devboy-mcp/src/agent_safety.rs`. A test should assert that every
    error variant maps to a remediation, so a newly added error cannot ship
    without one.
  - `crates/devboy-secrets-agent/src/socket.rs` — §7's self-checks: read the
    client PID (`SO_PEERCRED` / `LOCAL_PEERPID` /
    `GetNamedPipeClientProcessId`) and walk the daemon's own parent chain to
    detect an ancestor-client (check A); at startup, verify reparenting to the
    init system (check B) and the absence of a controlling terminal (check C).
    The result is a computed `trust_level` published through `secrets_status()`
    — never a configured claim. Checks must be structural: no list of vendor
    process names, per CI guard #243. A and B are **fatal by default** (refuse
    the connection after sending `DaemonUntrusted`; refuse to start with the
    correct launch command in the message); C warns. Only
    `DEVBOY_INSECURE_ALLOW_UNTRUSTED_DAEMON=1` downgrades A and B to warnings,
    and it must not suppress the repeated warning, alter `trust_level`, or
    re-add `"totp"` to `available_methods`. The integration-test harness is the
    primary consumer of that override and should set it explicitly rather than
    inheriting it.
  - `crates/devboy-cli/src/doctor/` — report the achieved trusted-path level
    (separate UID / independent lifecycle / agent-parented), the active
    profile, and any legacy keychain entries awaiting migration.
- **Documentation (planned):**
  - `docs/guide/secrets/agent-protocol.md` — add the new tools, the full
    error → remediation table of §8, and the negative contract (never request
    the passphrase, never start the daemon, never work around a missing
    secret, never retry past the stated backoff).
  - `docs/guide/secrets/local-vault.md` — TOTP enrollment, unlock-window
    profiles, keyfile setup, audit-log rotation, version history and recovery.
  - `docs/guide/secrets/threat-model.md` — the §7 process model, the three
    levels, and the honest limit, in one place users can be pointed at.
  - `docs/guide/secrets/ci.md` — the env-only contract: which variables to
    set under both conventions, what is unavailable and why, and how to make
    the mode explicit rather than relying on heuristics.
  - New BDD scenarios
    `docs/guide/secrets/scenarios/totp-unlock-and-audit.feature` and
    `docs/guide/secrets/scenarios/ci-env-only.feature` — the latter asserting
    that no prompt is rendered, no keychain or daemon is contacted, writes
    fail explicitly, and both naming conventions resolve.

## References

- [ADR-005](./ADR-005-credential-storage.md) — keychain-as-primary with an env
  fallback; §6 partially replaces its store decision, keeping the rest.
- [ADR-019](./ADR-019-secret-string-discipline.md) — `SecretString` end-to-end.
- [ADR-020](./ADR-020-secret-manifest-and-alias-resolution.md) — manifest, path
  namespace, alias resolution (`@secret:<path>`), validation (§6 liveness).
- [ADR-021](./ADR-021-external-secret-sources.md) — source router and
  `SecretSource::validate`.
- [ADR-023](./ADR-023-secret-store-ux-layer.md) — local vault, daemon,
  `AgentSafeReply`, the 15-minute idle policy this ADR generalizes.
- [RFC 4226](https://datatracker.ietf.org/doc/html/rfc4226) — HOTP, the
  construction TOTP builds on.
- [RFC 6238](https://datatracker.ietf.org/doc/html/rfc6238) — TOTP; §5.2 is the
  basis for the replay guard in §1.
- [RFC 8439](https://datatracker.ietf.org/doc/html/rfc8439) — ChaCha20-Poly1305.
- [Yama LSM](https://docs.kernel.org/admin-guide/LSM/Yama.html) —
  `ptrace_scope`, the kernel policy §7's level 2 depends on.

---

## Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-08-04 | Andrei Mazniak | Initial draft — TOTP unlock envelope, configurable unlock window, agent-mediated `secrets_unlock` / `secrets_validate`, encrypted audit log-store with enforced value→alias scrub. |
| 2026-08-11 | Andrei Mazniak | **Reframed §1, §6; added §7.** §1: TOTP is no longer a passphrase replacement — `totp_secret` moves from the OS keystore into the encrypted vault + daemon memory, with a reserved slot, replay guard, and an explicit dependency on §7; the strength argument ("as strong as its keystore, never the 6 digits") is now stated as a rule. §6: keychain **demoted to opt-in** (`[secrets.keychain] enabled`, default `false`) instead of removed, with a per-platform table showing it only exceeds `0600` on macOS; `Envelope::Keyfile` added for unattended cold start; CI mode gains a real consumer; an implementation note records that the runtime default lives in `ChainStore`, not in `sources.toml`. §2: split into `convenient` / `strict` profiles, because a long window and per-call approval genuinely conflict. §7 (new): trusted-path process model — daemon under its own UID, daemon-rendered prompts, must not be a child of the agent, platform primitives, macOS keychain re-entering as anti-tamper, and the honest credential-vs-authorization limit. Threat model, Decision, Consequences, Alternatives 6–8 and Implementation updated to match. |
| 2026-08-11 | Andrei Mazniak | **§6: CI / env-only mode promoted to a first-class contract.** Spelled out as a table what is and is not active when the environment is the sole source (no vault, daemon, keychain, prompt, approval, audit log or version history), so a pipeline can never hang on an invisible prompt or a D-Bus call. Pinned a five-step variable-resolution order that keeps **both** naming conventions working — ADR-005's `DEVBOY_GITHUB_TOKEN` / unprefixed `GITHUB_TOKEN` alongside ADR-021's `DEVBOY_SECRET__<PATH>` — since routing CI through only the latter would break existing pipelines silently. Required fail-fast behaviour (errors list every variable tried; writes return an explicit read-only error instead of disappearing into `MemoryStore`; vault-dependent MCP tools return `NotAvailableInCiMode`). Confirmed that heuristic CI variables raise a `doctor` notice but never flip the mode. Decision (6), Risks, Implementation and the docs plan updated to match. |
| 2026-08-11 | Andrei Mazniak | **New §8: actionable errors.** Every failure reply now carries a `remediation { actor, action, user_message, retryable, retry_after_seconds }`, where `actor` tells the agent whether it can resolve the problem or must stop and fetch a human. Adds the full error → remediation table, and a **negative contract** covering the guesses that silently dismantle the other sub-decisions: never request the passphrase, never start the daemon (a self-spawned daemon is `ptrace`-able and voids §1/§7), never work around a missing secret via env/dotfiles/history, never retry past the stated backoff. Surfaces the ADR-020 manifest metadata (`retrieval_url`, `required_scopes`, `rotation_method`) that has never had a consumer, so `NotProvisioned` tells the user which token to create, with which scopes, and where. `user_message` is daemon-authored, which also closes the prompt-injection concern already listed in Risks. Sub-decision count 7 → 8; gap 6 added to Context; Decision (8), Consequences and Implementation updated. |
| 2026-08-11 | Andrei Mazniak | **§7: the two credential flows made explicit.** The passphrase travels user → daemon with the agent outside the path entirely; a TOTP code travels user → agent → daemon with the agent as transport. States the rule the asymmetry follows from — *a credential may cross the agent surface only if it is ephemeral **and** cannot perform a cold start* — with the property table showing both conditions are load-bearing. Adds a requirement that was missing: the passphrase prompt must not render into a PTY the agent controls, since an agent that spawned the shell holds the master and reads non-echoed input regardless of which process printed the prompt (`doctor` must grade such a setup as level 3). Documents that the agent learns of an unlock by polling `secrets_status()` — there is deliberately no callback into a flow it does not participate in. Adds the user-facing rule *devboy never asks for your vault passphrase in an agent conversation*, and records the residual risk that the agent can solicit a TOTP code prematurely. Three matching entries added to Risks. |
| 2026-08-11 | Andrei Mazniak | **§7: the daemon now enforces its own provenance instead of relying on an operational rule.** Three structural self-checks: (A) on every connection, read the client PID and walk the daemon's own parent chain — a client found there can `ptrace` the daemon, so `secrets_unlock` is refused for it with `DaemonUntrusted`; (B) at startup, verify reparenting to the init system; (C) verify no controlling terminal. Checks are deliberately structural rather than nominal — the daemon asks "can my caller `ptrace` me", never "is my ancestor a coding agent", which would need a vendor process-name list that CI guard #243 forbids and that would fail on the first agent not listed. On failure the daemon degrades and announces rather than crashing: `trust_level` becomes a value it computes and publishes through `secrets_status()`, not a claim the documentation makes. Notes that double-forking defeats check A but is self-defeating — `ptrace_scope` evaluates descent at call time, so an orphaned daemon is no longer `ptrace`-able by its starter — and that launch-time environment control (`LD_PRELOAD`, substituted binary) is a code-integrity problem the check does not address. §8 gains the `DaemonUntrusted` row and a strengthened negative contract. |
| 2026-08-11 | Andrei Mazniak | **§7: provenance checks are fail-closed by default.** The previous revision had the daemon degrade and keep running, which is the outcome §7 spends its length arguing against — it preserves the appearance of a guarantee that no longer holds. Checks A (client is an ancestor) and B (parent is not the init system) are now **fatal**: A sends `DaemonUntrusted` with its §8 remediation *and then closes the connection*, refusing the client entirely rather than only denying the TOTP path; B refuses to start, exiting non-zero with the platform's correct launch command. Check C (controlling terminal) stays a warning, since holding a TTY is a hint about launch method rather than a `ptrace` capability. Adds `DEVBOY_INSECURE_ALLOW_UNTRUSTED_DAEMON=1` for integration tests and hand-started local daemons, which downgrades A and B to warnings but never suppresses them, never alters the reported `trust_level`, and never re-adds `"totp"` to `available_methods` — the override changes what is *permitted*, never what is *claimed*. Its weakness is recorded rather than left to be discovered: an agent that starts the daemon controls its environment and can set the variable, so the override guards against accidental misconfiguration and good-faith agents, not hostile ones. Two matching entries added to Risks. |
