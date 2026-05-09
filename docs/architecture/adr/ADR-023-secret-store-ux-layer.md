---
id: ADR-023
title: Secret store UX layer — local vault, daemon, native UI, agent protocol, rotation, pattern catalogue, onboarding skill
status: proposed
date: 2026-05-09
deciders: ["Andrei Mazniak"]
tags: ["security", "secrets", "ui", "agent", "rotation", "patterns", "skills"]
supersedes: null
superseded_by: null
---

# ADR-023: Secret store UX layer

## Status

**proposed**

This ADR is intentionally an **umbrella**. The conventions in
[`INDEX.md`](./INDEX.md) prefer one decision per ADR, and the eight
sub-decisions here could be split into eight separate files. They are
not, because they describe a **single coherent layer** sitting above
[ADR-021](./ADR-021-external-secret-sources.md): the components are
designed against each other (the daemon serves the UI, the UI is what
the agent triggers when it cannot get a value, the rotation flow uses
both, the pattern catalogue feeds all of them, the onboarding skill
walks a user through the whole stack at once). Splitting them would
multiply cross-references without adding clarity. If review surfaces
that a sub-decision should evolve independently — for example, the
pattern catalogue grows beyond the secret store and is used by other
subsystems — that section is a candidate for promotion to its own ADR.

## Context

[ADR-019](./ADR-019-secret-string-discipline.md), [ADR-020](./ADR-020-secret-manifest-and-alias-resolution.md),
and [ADR-021](./ADR-021-external-secret-sources.md) together form the
**transport layer** of the secret framework: how secrets are typed in
memory, how they are named and discovered, and where their values live.
None of those layers say what the user **sees**, what the **agent** is
allowed to do, how a token gets **rotated** in practice, or how a new
contributor **gets set up** for the first time.

In practice this leaves four concrete gaps that the transport layer
cannot close on its own:

1. **No fallback for fully offline / no-keychain machines.** ADR-021
   ships an env-store, but env-store is a CI shape, not a developer
   shape. A developer working off-network on a Linux box without
   Secret Service has nowhere to put a passphrase-protected store.
2. **No way for the user to enter a value without the agent seeing
   it.** The agent that is helping the user set up the project is
   the same channel through which the user would type a secret. A
   second, agent-bypassing UI is required.
3. **No rotation flow.** The transport layer can detect "this token
   expires in three days" but cannot guide the user through the
   rotation: open the right URL, accept the new value, validate, and
   record the rotation. Without a flow, rotation reminders become
   notification fatigue.
4. **No agent-mediated provisioning.** When the agent finds that a
   project needs a credential that is not yet provisioned, it can
   neither read existing values (by design) nor accept new ones
   (it is the leak surface). It needs a typed protocol for asking
   the user to provision a path through the UI.

This ADR introduces the **UX layer**: an encrypted local vault for
the offline-fallback case, a daemon to manage its unlocked state, a
native UI (TUI + GUI) that the user interacts with directly, a
manual-assisted rotation flow built on top, a pattern catalogue
shared with the OTLP sanitizer (#240/#242), an MCP protocol for
agent-mediated provisioning, and an onboarding skill that walks a
user through the stack on first run.

### Threat model alignment

This ADR inherits the threat model of [ADR-020](./ADR-020-secret-manifest-and-alias-resolution.md):
the framework protects against accidental leakage by humans, by
agents acting in good faith, and by routine tooling. It does not
claim isolation against a malicious agent that can spawn shells.
The local vault and the daemon raise the bar (encrypted at rest,
zeroized in memory, segmented unlock), but they do not change the
boundary — a process running as the user can still read the
keychain and `/proc/self/environ`. The UX layer's stronger claim
is operational, not cryptographic: the user types secrets into a
UI that the agent does not mediate, the agent never receives
values, and rotation happens on a cadence rather than on demand.

## Decision

> **Decision:** A user-facing UX layer sits above the router from
> [ADR-021](./ADR-021-external-secret-sources.md), built around an
> encrypted local vault, a single-purpose daemon, a native TUI/GUI,
> an agent provisioning protocol over MCP, a manual-assisted
> rotation flow, a shared pattern catalogue, and an onboarding skill.
> The agent never sees secret values; the user never types secrets
> through the agent.

The decision has eight parts.

### 3.1 Encrypted local vault

The local vault is one of the built-in sources from
[ADR-021](./ADR-021-external-secret-sources.md) section 8. It is the
fallback for environments without an OS keychain and for users who
explicitly want a portion of their namespace stored locally,
encrypted, behind a passphrase.

#### File layout

The vault lives at `~/.devboy/secrets/local-vault.dvb`. Its layout is
plaintext-header plus per-entry AEAD ciphertexts:

```
HEADER (plaintext, fixed-width fields):
  MAGIC          [4]   = b"DVB1"
  VERSION        [1]   = 0x01
  KDF_PARAMS     [16]  = Argon2id (m_cost, t_cost, p_cost, salt-len)
  SALT           [32]  = random per-vault, used by passphrase envelope

UNLOCK_ENVELOPES (TOML-serialised, each is an AEAD-wrap of the vault-key):
  [[envelope]]
  kind = "passphrase"
  argon2_salt    = "<32B base64>"
  argon2_params  = { m = 65536, t = 3, p = 1 }
  wrapped_key    = "<AEAD(vault_key, key=Argon2id(passphrase, salt))>"

  [[envelope]]
  kind = "keychain"           # macOS-only; optional
  keychain_account = "dev.devboy.secrets.vault.macos-touchid"
  wrapped_key      = "<AEAD(vault_key, key=Keychain-protected key)>"

  [[envelope]]
  kind = "recovery"
  bip39_salt    = "<32B base64>"
  wrapped_key   = "<AEAD(vault_key, key=HKDF(bip39_seed))>"

ENTRIES_INDEX (plaintext TOML, metadata only):
  [[entry]]
  path           = "team/gitlab/token-deploy"
  nonce          = "<24B base64>"
  ct_offset      = 0
  ct_length      = 312
  description    = "..."
  retrieval_url  = "..."
  expires_at     = "2026-08-01"
  last_rotated_at = "2026-05-02"
  pattern_id     = "gitlab-pat"

  [[entry]]
  ...

AEAD_BLOBS:
  contiguous concatenation of per-entry ciphertexts; each entry is
    ChaCha20-Poly1305(
      plaintext     = value (utf-8 bytes),
      key           = vault_key,
      nonce         = entry.nonce,
      associated_data = entry.path utf-8 bytes
    )
```

Critical invariants:

- **Envelope encryption.** A single random `vault_key` (32 bytes)
  encrypts every entry. The header carries one or more *envelopes*,
  each of which independently wraps `vault_key` under a different
  unlock method. Adding a new unlock path (Touch ID, recovery
  phrase) is a write to the header only and never touches the
  per-entry ciphertexts.
- **AAD includes path.** Per-entry AEAD uses the path as
  associated data. A tampering attempt that swaps the ciphertext
  blob from `team/gitlab/...` under the index entry for
  `personal/github/...` fails decryption. This closes a class of
  swap attacks that pure encryption-without-AAD does not.
- **Plaintext metadata is intentional.** `description`,
  `retrieval_url`, `expires_at` are all readable without an unlock
  step. The discovery and rotation-reminder flows need this; the
  threat model already grants any local process read access to
  metadata, and encrypting it would gate every `secrets list` on a
  PIN prompt for no real benefit.
- **No file-level integrity over the whole vault.** Each entry is
  independently authenticated; there is no Merkle tree or
  whole-file MAC. A truncation attack on the entry table is
  detected at parse time (TOML parse error or entry-count
  mismatch), not cryptographically. This is acceptable because the
  vault file is single-writer (the daemon, see section 3.3) and
  protected by filesystem permissions.

#### Algorithms

- **AEAD:** ChaCha20-Poly1305 (RFC 8439). Picked over AES-GCM
  because it does not require AES-NI and runs constant-time on all
  targets (including ARM Linux); RustCrypto provides a vetted
  pure-Rust implementation (`chacha20poly1305`).
- **KDF:** Argon2id with `m_cost = 65536` (64 MiB), `t_cost = 3`,
  `p_cost = 1` for the passphrase envelope. Tuned to ≈250 ms on a
  2024-class laptop; tuneable per-vault if a host is too slow.
  The recovery envelope uses HKDF over the BIP39 seed (no
  brute-force resistance needed; the seed has 256 bits of entropy
  by construction).
- **CSPRNG:** the `getrandom` crate, the same source used by the
  rest of the workspace.

#### Crate

A new workspace member, `crates/devboy-vault-crypto/`, exposes the
algorithms behind a small API:

```rust
pub struct Vault { /* unlocked state */ }
impl Vault {
    pub fn open(path: &Path, unlock: UnlockMethod) -> Result<Self>;
    pub fn create(path: &Path, methods: &[InitialUnlock]) -> Result<Self>;
    pub fn add_envelope(&mut self, kind: EnvelopeKind, secret: SecretString) -> Result<()>;
    pub fn get(&self, path: &str) -> Result<Option<SecretString>>;
    pub fn put(&mut self, path: &str, value: SecretString, meta: EntryMetadata) -> Result<()>;
    pub fn rotate(&mut self, path: &str, new: SecretString) -> Result<()>;
    pub fn delete(&mut self, path: &str) -> Result<()>;
    pub fn list(&self) -> impl Iterator<Item = &EntryMetadata>;
}
```

The vault unlocked state holds `vault_key` in a `secrecy::SecretBox<[u8; 32]>`
that zeroizes on drop (per ADR-019). The `Vault` instance is owned by
the daemon (section 3.3); CLI commands and the UI talk to the daemon,
not to this crate directly.

### 3.2 Authentication and recovery

The vault accepts three independent unlock methods:

- **Passphrase** — minimum 12 characters. Required at vault
  creation; cannot be removed.
- **Keychain (Touch ID)** — macOS only. The user runs
  `devboy secrets vault add-keychain-unlock`, the CLI generates a
  random 32-byte key, stores it in the macOS keychain under
  `kSecAttrAccessControlBiometryAny | kSecAttrAccessControlUserPresence`
  (Touch ID required, fallback to user password), and adds a
  `keychain` envelope. Future unlocks via Touch ID don't require
  the passphrase.
- **Recovery (BIP39)** — generated at vault creation. The CLI
  derives a recovery key from a 24-word BIP39 phrase, wraps
  `vault_key` under it, and shows the phrase **once** with explicit
  acknowledgement (`Type 'I have written it down' to continue`).
  The phrase is never stored.

A user who loses both the passphrase and the keychain unlock can
recover with `devboy secrets vault recover`, which prompts for the
24-word phrase, decrypts `vault_key`, prompts for a new passphrase,
and rewrites the passphrase envelope. A user who loses the
passphrase, the keychain unlock, **and** the recovery phrase has no
recovery path; the documentation says so explicitly.

#### Why not just keychain?

On macOS the keychain alone is sufficient. The reason the local
vault exists at all is for the cases that the keychain does not
cover (headless Linux, containers without `loginctl`, a user who
explicitly wants some paths off-keychain). The keychain-as-unlock
path is an ergonomic add-on for macOS users who want both stores
under a single biometric flow.

### 3.3 Daemon: `devboy-secrets-agent`

The daemon owns the unlocked `Vault` instance. It speaks JSON-RPC
2.0 over a UNIX domain socket at `~/.devboy/secrets/agent.sock`,
mode `0600`, owned by the running user. Windows is out of scope for
the first release; on Windows the local-vault source falls back to
a per-call unlock until a named-pipe transport ships.

#### Lifecycle

The daemon supports two startup modes:

- **On-demand (default).** The CLI checks for a live socket; if
  none is found, it spawns the daemon as a detached child and
  hands the user the unlock UI before issuing the first
  `secret.get`. After **15 minutes** of idle time, the daemon
  zeroizes `vault_key` and exits. This mode requires no install
  step; the only configuration is the vault file itself.
- **Persistent (opt-in).** `devboy secrets agent install` writes a
  launchd `LaunchAgent` plist on macOS or a `systemd --user`
  service on Linux. The daemon starts on user login and stays
  running, but the *unlocked state* still expires on idle and on
  SIGTERM. The user trades a process always running for not
  having to re-enter the unlock method between CLI sessions.

The lifecycle code is identical in both modes; only the supervision
differs. In both modes the daemon enforces:

- **Idle timeout:** the unlocked vault key is zeroized 15 minutes
  after the last successful `secret.get` or `metadata.update`.
  Configurable per user in `~/.devboy/config.toml`.
- **Eager re-lock:** `vault.lock` zeroizes immediately. Used by
  the UI's "lock now" button and by the rotation flow after a
  successful rotation.
- **SIGTERM zeroization:** the daemon traps SIGTERM, zeroizes,
  flushes pending writes, and exits within 10 seconds.
- **Authenticated operations.** `secret.put`, `secret.rotate`, and
  `vault.add-envelope` always require a fresh PIN/passphrase
  prompt independent of the daemon's current unlocked state. This
  is the "hybrid" model from design review: reads benefit from
  daemon caching, writes do not.

#### Wire protocol

JSON-RPC 2.0, the same shape as MCP. Methods:

```
vault.unlock(method)      params: { kind: "passphrase" | "keychain" | "recovery", secret: string }
                          result: { unlocked_at: timestamp, expires_at: timestamp }

vault.lock                params: {}
                          result: { locked: true }

vault.status              params: {}
                          result: { state: "locked" | "unlocked",
                                    unlocked_at?: timestamp,
                                    expires_at?: timestamp,
                                    available_methods: [...] }

secret.get(path)          params: { path: string }
                          result: { value: string }      // SecretString on the wire
                                | { error: "Locked" | "NotFound" }

secret.list(filter?)      params: { filter?: { scope?, provider?, status? } }
                          result: { entries: [{ path, status, expires_at?, ... }] }
                          // Never returns values.

secret.put(path,          params: { path, value, meta, fresh_unlock: { kind, secret } }
           value, meta)   result: { ok: true } | { error: "BadUnlock" }

secret.rotate(path, new)  params: { path, new_value, fresh_unlock: { ... } }
                          result: { ok: true, last_rotated_at }

metadata.update(path,     params: { path, fields }
                fields)   result: { ok: true, applied_fields: [...] }
                          // Does not require unlock; metadata is plaintext.
```

#### Security boundary

The socket file is created with `umask 077` and verified at connect
time: the daemon checks the connecting peer's UID through
`SO_PEERCRED` (Linux) or `LOCAL_PEERCRED` (macOS). A connection
from a different UID is rejected. This is defence in depth on top
of the filesystem permissions; it costs nothing and catches a
subset of misconfigurations.

The daemon **does not** hand out `vault_key`. Every read goes
through `secret.get(path)` which returns one decrypted value at a
time. A compromised socket-listener that intercepts the wire
between the CLI and the daemon learns the secrets that the
listener was specifically targeting, not the entire vault.

### 3.4 Native UI

The UI is a `devboy secrets ui` subcommand of the main `devboy`
binary. There is one binary; no separate `.app` bundle for the
first release.

The subcommand has two backends:

- **TUI** — `ratatui`, runs in any terminal. Used by default in
  contexts where a full GUI is impractical (SSH sessions,
  containers, headless screens).
- **GUI** — `egui`, runs in a native window. Used by default
  when `$DISPLAY` / `$WAYLAND_DISPLAY` is set on Linux or when
  running on macOS / Windows. `egui` was picked over `iced`
  because its immediate-mode API matches the `ratatui` mental
  model (one render function per frame), making the two backends
  share the same view code.

Backend selection: `--tui` / `--gui` flags, falling through to
`devboy secrets ui` which auto-detects.

#### MVP views

Four views ship in the first release:

- **Inventory** — a sortable, filterable table of every secret
  the active context can see. Columns: path, status
  (`provisioned` / `expiring` / `missing` / `format-invalid`),
  routed source, `expires_at`, provider, scope. Filters by scope,
  provider, status. **No values are ever shown.**
- **Provision / rotation dialog** — opened by an agent
  request_provision call (section 3.7) or by `devboy secrets
  provision <path>` directly. Shows the path's metadata, a
  `[Open retrieval URL]` button that opens the user's browser at
  `retrieval_url`, a hidden input for the new value, and a
  `[Validate & save]` button that runs the format check, runs the
  liveness check, writes through the daemon (with a fresh PIN
  prompt for the local-vault), and updates `last_rotated_at`. The
  same dialog handles initial provision and rotation; a `mode`
  parameter changes the title and the destructive-confirm
  behaviour.
- **Edit metadata** — editable fields: `description`,
  `retrieval_url`, `rotate_every_days`, `expires_at`, `pattern_id`.
  Dirty fields are highlighted; `[Save]` writes through the
  daemon's `metadata.update`. A subview `[Apply agent suggestion]`
  shows the diff from a `secrets.propose_metadata` call (section
  3.7) and lets the user accept all / reject all / accept per
  field.
- **Discovery import** — connects to a configured 1Password vault
  or HashiCorp Vault mount, lists item titles (no values) through
  the source's `list()` method, suggests `<scope>/<provider>/<purpose>`
  paths via the algorithm in section 3.6, and asks the user to
  confirm or edit each mapping. **Values are never copied out
  of the upstream**; the import only registers paths in the
  global index with `source` and `reference` set so future reads
  resolve through the source.

The UI talks to the daemon, never to the keychain or the local
vault directly. This keeps the security boundary uniform: the
daemon is the only process that holds `vault_key`.

### 3.5 Manual-assisted rotation

Rotation is **assisted**, not automatic. The decision to call
provider rotate-APIs is deferred to a future ADR; this release
covers the case where the user changes a token in the upstream UI
and `devboy-tools` records the change.

#### Flow

`devboy secrets rotate <path>` (or the agent's
`secrets.request_rotation(path)` call):

1. The CLI / MCP server calls `secrets.describe(path)` to load
   metadata. If `retrieval_url` is set, the CLI opens it in the
   user's default browser.
2. The UI's provision/rotation dialog opens in `mode = "rotation"`
   with the existing path metadata.
3. The user pastes the new value into the hidden input.
4. The UI runs format validation against `format_regex` (or the
   pattern's regex if `pattern_id` is set). On format failure the
   dialog shows the mismatch and does not save.
5. The UI runs liveness validation through the provider plugin.
   On liveness failure the dialog asks for confirmation
   ("upstream rejected this token; save anyway?") with a default
   of *no*. If the user confirms anyway, the rotation proceeds
   but a warning is recorded.
6. The UI writes the new value through the daemon's
   `secret.rotate` (which requires a fresh PIN if the route
   targets the local-vault) and updates `last_rotated_at` in the
   index.
7. On success, the agent (if it was the caller) receives
   `{ ok: true, last_rotated_at }`. The agent never sees the
   value.

#### Rotation cadence and reminders

`devboy doctor` warns at `< 7` days before `expires_at` (default,
configurable). If the `notify` skill is wired up, the warning is
also delivered there. Rotation is otherwise a quiet background
fact; the system does not page the user.

#### What this ADR does **not** ship

Provider-driven rotation (calling `gitlab.users.create_personal_access_token`,
`aws iam create-access-key`, etc.) is deferred. The reasoning:
provider rotation APIs are heterogeneous (some require existing
credentials, some require browser interaction, some don't exist),
and the assisted flow above covers 100% of providers. A future ADR
("ADR-2N: Provider-driven secret rotation") will add per-provider
rotators behind the same `secrets.request_rotation` MCP call,
falling through to manual rotation when the provider has no
auto-rotate path.

### 3.6 Pattern catalogue (`devboy-secret-patterns`)

A new workspace crate, `crates/devboy-secret-patterns/`, holds the
canonical descriptions of secret types: GitHub PATs, GitLab tokens,
AWS access keys, OpenAI API keys, JWTs, Slack tokens, Vault tokens,
and so on. The crate is shared with the OTLP sanitizer (#240) and
the `otel scan` auditor (#242) so all three subsystems see the
same regex and severity, but the patterns are written from the
secret-store side first.

#### Layered trait

```rust
pub trait SecretPattern: Send + Sync {
    fn id(&self) -> &str;
    fn display_name(&self) -> &str;
    fn format_regex(&self) -> &Regex;
    fn severity(&self) -> Severity;

    // Optional layers — present if the pattern supplies them
    fn metadata(&self) -> Option<&PatternMetadata>;
    fn rotation(&self) -> Option<&RotationSpec>;
    fn liveness(&self) -> Option<&LivenessSpec>;
}

pub struct PatternMetadata {
    pub provider_id: &'static str,
    pub retrieval_url_template: &'static str,
    pub default_expiry_days: Option<u32>,
    pub scopes_hint: Vec<&'static str>,
}

pub struct RotationSpec {
    pub method: RotationMethod,            // Manual | ProviderUi { url } | ProviderApi { reserved }
}

pub struct LivenessSpec {
    pub kind: LivenessKind,                // Http { url, method, auth, expect_status }
}
```

The OTLP sanitizer and `otel scan` consume only `format_regex` +
`severity`. The secret store consumes everything available; missing
optional fields are user-fillable in the UI's edit-metadata view.

#### Built-in catalogue + user extension

The crate ships a catalogue of ~30 well-known patterns hard-coded
behind `SecretPattern` impls. Users may extend the catalogue by
dropping TOML files into `~/.devboy/secrets/patterns.d/`. Each TOML
file declares one or more patterns:

```toml
[[pattern]]
id              = "internal-mfa-token"
display_name    = "Internal MFA Service Token"
format_regex    = "^mfa_[A-Z0-9]{40}$"
severity        = "high"
provider_id     = "internal"
retrieval_url_template = "https://mfa.example.internal/tokens"
default_expiry_days = 180
```

User-supplied patterns are loaded at process start and merged with
the built-in catalogue. A user-supplied pattern with the same `id`
as a built-in shadows the built-in (with a `doctor` warning so the
user knows shadowing happened).

#### Why a new crate

The catalogue is small but central, and three consumers want to
import it. Putting it in `devboy-storage` would force the OTLP
sanitizer to depend on the storage layer just to read regexes;
putting it in `devboy-otel-sanitizer` would force the storage layer
to depend on OTLP. A small dedicated crate is the cleanest split.

### 3.7 MCP agent provisioning protocol

The MCP server (from [ADR-021](./ADR-021-external-secret-sources.md)
section 6 / [ADR-005](./ADR-005-credential-storage.md)) exposes the
following typed tools to the agent. **There is no `secrets.get`
tool exposed to the agent surface.** The only legitimate path for
a value to reach the agent is through a high-level provider tool
(e.g. `gitlab.create_merge_request`) where the resolution happens
server-side and the agent receives only the operation's outcome.

```
secrets.list(filter?)
  → [ { path, status, expires_at?, source_name, capabilities_hint? } ]
  Reads the active context's manifest. Never returns values.

secrets.describe(path)
  → { path, metadata: PatternMetadata?, status, expires_at?, last_rotated_at? }
  Reads the global index + per-project overrides. Never returns the value.

secrets.request_provision(path)
  → { request_id, status: "pending" }
  Opens the provision dialog (section 3.4) on the user's UI.
  The agent polls poll_status to track the result.

secrets.request_rotation(path)
  → { request_id, status: "pending" }
  Same flow with mode=rotation.

secrets.poll_status(request_id)
  → { status: "pending" | "ok" | "cancelled" | "expired",
      expires_at?, last_rotated_at? }
  Default timeout for a pending request is 5 minutes; after
  that the request_id resolves to "expired".

secrets.propose_metadata(path, fields)
  → { request_id, status: "pending" }
  Opens the edit-metadata view with a diff of fields to apply.
  The user accepts/rejects per field; the agent receives the
  applied subset through poll_status.

secrets.propose_new_path(suggested_path, metadata)
  → { request_id, status: "pending" }
  Opens a registration dialog. The user can accept the suggested
  path, edit it, or reject. Used by the agent when it detects a
  project consuming a token that has no manifest entry.
```

#### "Agent never sees value" as an enforced invariant

The MCP server's tool-registration code asserts at startup that no
registered tool returns a `SecretString` directly to the agent.
This is enforced by:

- A trait bound on the tool result type: tools may return
  `serde_json::Value` plus a typed wrapper, but the wrapper does
  not implement `Serialize` if it contains a `SecretString`
  (per ADR-019).
- A pre-commit grep gate in `crates/devboy-mcp/` that flags any
  use of `.expose_secret()` outside of the high-level provider
  tools' internal HTTP-call construction.
- A negative test in `crates/devboy-mcp/tests/` that constructs
  every registered tool, drives it with mock inputs, and asserts
  no tool's result serialises any `SecretString`-derived value.

These checks are aspirational hard guarantees: a contributor who
adds a tool that *would* return a value receives a compile error
or a CI failure, not a code review comment they might miss.

### 3.8 Onboarding skill `setup-secrets`

A new skill at `skills/setup-secrets/skill.md` walks the user
through the framework on first run. The existing `setup` skill
gains a `secrets bootstrap` step that delegates to `setup-secrets`
when a project has a `.devboy/secrets.toml` manifest with at least
one required path; otherwise the step is a no-op so projects that
don't use secrets pay no cost.

#### Flow

The skill is **idempotent**: a re-run resumes from the first
incomplete step. State is recorded in
`~/.devboy/secrets/setup-state.toml`.

1. **Vault state.** Check `local-vault.dvb`. If absent and the
   keychain is available, skip to step 4 (the user does not need
   a local vault). If absent and the keychain is not available,
   proceed to step 2.
2. **Create vault.** Prompt for a passphrase (min 12 chars,
   confirmed twice). Generate a 24-word recovery phrase, display
   it once with explicit acknowledgement, do not store it. Write
   `local-vault.dvb` with passphrase + recovery envelopes.
3. **Optional Touch ID.** On macOS, ask the user whether to add
   a Touch-ID unlock. If yes, add a keychain envelope.
4. **Configure routing.** Walk through the candidate sources.
   For each available source, register it in `sources.toml`;
   for each unavailable source (e.g. `op` not installed),
   record a skipped status with a one-line "install with `brew
   install 1password-cli`" hint. The keychain (or local-vault)
   is configured as `[default]` automatically.
5. **Walk required secrets.** For each path in
   `.devboy/secrets.toml`'s `required` list:
   - Run `secrets.describe(path)` to fetch metadata.
   - If already provisioned and not expiring, skip.
   - Otherwise, open the provision dialog (section 3.4) and
     record the outcome.
6. **Walk optional secrets.** Same flow with informational
   skipping of missing values.
7. **Run validation.** Format + liveness for every required
   path. Failures are surfaced inline; the user can re-enter or
   skip.
8. **Run `doctor`.** Expected to pass. If `doctor` fails, the
   skill loops back to the failing step with the specific path
   and reason.

Each step emits one structured message to the agent
(`{step, status, summary, next_options}`), and the agent presents
the user with explicit `next` / `skip` / `abort` choices. The
skill does **not** continue to the next step on its own; it
always waits for the user.

#### Documentation

`docs/guide/secrets/onboarding.md` (planned) describes the same
flow for users running `devboy secrets bootstrap` manually without
the agent. The skill and the manual flow share state through
`setup-state.toml`; mixing them is supported.

## Consequences

### Positive

- ✅ **Headless Linux works without external infrastructure.**
  The local vault is a real, encrypted store; "no keychain, no
  Vault, no 1Password" is now a supported configuration.
- ✅ **The agent never sees secret values.** The MCP surface is
  designed around request/poll, the local-vault never hands out
  `vault_key`, and the high-level provider tools resolve
  credentials server-side. The `secrets.get` tool simply does
  not exist on the agent surface.
- ✅ **Rotation is a guided action, not a notification.** The
  rotation dialog opens the right URL, validates the new value,
  and records the rotation in one flow — the user does not have
  to remember to update `last_rotated_at`.
- ✅ **Pattern catalogue is shared infrastructure.** The same
  regex and severity drive secret-store validation, OTLP
  sanitization (#240), and OTEL artifact scanning (#242). One
  source of truth, three consumers.
- ✅ **Onboarding is data, not lore (continued from ADR-020).**
  The `setup-secrets` skill walks every required path through a
  consistent UI, with idempotent resume on interruption.
- ✅ **One binary, two UIs.** TUI on `ratatui` and GUI on `egui`
  share view code; the user gets the right interface for their
  context without an extra install step.

### Negative

- ❌ **One more long-running process.** The on-demand daemon is
  the smallest version of this; the persistent install adds a
  launchd / systemd service to manage. Both are testable but the
  surface is larger than "stateless CLI".
- ❌ **An umbrella ADR.** Eight sub-decisions in one file
  trades enforceability of the "single decision per ADR" rule
  for cohesion. Future evolution may need to split sections out
  (the pattern catalogue is the most likely candidate).
- ❌ **GUI dependencies in the main binary.** `egui` and the
  `winit` stack add ~3 MB to the release binary. Users who only
  use the TUI pay this cost. Mitigation: feature-gate the GUI
  behind `default-features = ["gui"]` in `devboy-cli`'s
  `Cargo.toml`; CI binaries can opt out.
- ❌ **Two unlock paths means two places for unlock bugs to
  hide.** A failure in either the passphrase or the keychain
  envelope must surface clearly; the daemon's `vault.unlock`
  result enumerates the failure cause.

### Risks

- ⚠️ **Lost recovery phrase = lost vault.** The user who loses
  passphrase + keychain unlock + recovery phrase has no recourse.
  **Mitigation:** the bootstrap flow shows the recovery phrase
  with explicit acknowledgement; the documentation calls this
  out; users on teams with external sources should route the
  primary tree through the external source and use the local
  vault only for paths they can afford to recreate.
- ⚠️ **Daemon survives a session it shouldn't.** A user logs
  out without the daemon receiving SIGTERM (a hard kill of the
  parent shell, for example) and the unlocked key persists in
  RAM until the OS reclaims the page. **Mitigation:** the
  Argon2id KDF cost makes brute force on the wrapped key
  expensive even after RAM acquisition; the idle timeout
  (15 min default) bounds the window; `vault.lock` is exposed
  for explicit re-lock.
- ⚠️ **A compromised egui dependency could log keystrokes.**
  GUI input goes through `winit` and `egui`'s text-edit widget.
  **Mitigation:** the dependency is pinned to a vetted
  version, audited at release; the TUI fallback is available
  for users who prefer to limit the GUI surface.
- ⚠️ **Pattern catalogue becomes a maintenance burden.** New
  providers, new token formats, new rotation patterns
  accumulate. **Mitigation:** the user-extension mechanism
  (`patterns.d/`) lets new patterns ship out-of-band; the core
  catalogue ships ~30 patterns covering the long-tail of
  GitHub / GitLab / AWS / GCP / OpenAI / Anthropic / Slack /
  Stripe / Vault / common JWT shapes.
- ⚠️ **Agent abuses `request_provision` for prompt injection.**
  A malicious prompt could ask the agent to call
  `request_provision` for a path that opens a dialog asking the
  user to enter another credential under a misleading
  `description`. **Mitigation:** the dialog renders metadata
  from the global index, **not** strings supplied by the agent;
  `propose_metadata` shows the proposed change as a diff, not
  as an applied state.

## Alternatives Considered

### Alternative 1: No local vault — keychain or external sources only

**Description:** Drop section 3.1 / 3.2 / 3.3 entirely; users on
headless machines without a keychain must use an env-store or set
up an external source.

**Why rejected:** The "I want to work off-network on a Linux box
without provisioning Vault" case is real for short-lived
projects, demos, and contractor laptops. Forcing those users to
either set up a full Vault or paste tokens into env vars is
exactly the friction this ADR is designed to remove. The local
vault is also useful as a deliberate choice for users who want
some paths off-keychain — the routing layer makes that a per-path
decision.

### Alternative 2: Keychain-only with no daemon

**Description:** Skip the daemon entirely; rely on the OS
keychain's own session management.

**Why rejected:** The keychain is one source among five
(ADR-021); the local-vault still needs to manage unlocked state,
and that state is the daemon. The same daemon serves the
biometric-prompt batching argument (an agent loop should not
trigger N keychain unlocks for N reads).

### Alternative 3: Sealed file with sops/age and no daemon

**Description:** Use sops or age over a TOML file and unlock
on each read.

**Why rejected:** Sops pulls in a backend (age, GPG, KMS) and
turns the unlock step into a per-read prompt. The daemon's
single unlock per session is the ergonomic difference; without
it the local vault is unusable for any flow that touches more
than one secret.

### Alternative 4: Web UI on localhost

**Description:** Replace the `egui` GUI with a localhost-bound
HTTP server and a browser frontend.

**Why rejected:** Browser frontends pull in CORS, certificate,
and clipboard concerns that a native widget set does not have.
The TUI fallback covers the "I am SSH'd into a server and want
to provision a secret" case; the browser would not help there.
A future companion web UI for team admin views is not ruled out
but is out of scope here.

### Alternative 5: Provider-driven auto-rotation in v1

**Description:** Ship per-provider rotators alongside the manual
flow.

**Why rejected:** Provider rotation APIs are heterogeneous and
each one is a separate research project (which credential rotates
which, what scopes are needed for the rotate call itself, what
happens to in-flight tokens during rotation). Manual-assisted
rotation covers 100% of providers today; provider-driven
rotation is best handled per-provider once we have rotation
telemetry from the manual flow.

### Alternative 6: Eight separate ADRs

**Description:** Split this ADR into ADR-023 through ADR-029
along the eight section boundaries.

**Why rejected:** The components are designed against each
other; review would have to thread through all eight to see
the whole. The "single decision per ADR" convention is a
default; this ADR is an explicit exception, marked as such in
the Status section, with the candidate-for-promotion path
named.

## Implementation

- **Issues:**
  - [#247](https://github.com/meteora-pro/devboy-tools/issues/247) — implementation, phased; the Phase 6 cluster splits along this ADR's section boundaries
  - [#240](https://github.com/meteora-pro/devboy-tools/issues/240) — OTLP sanitizer; consumes `devboy-secret-patterns`
  - [#242](https://github.com/meteora-pro/devboy-tools/issues/242) — OTEL scan; also consumes `devboy-secret-patterns`
  - To be filed — design refresh covering rewritten ADR-020/021 and this new ADR-023
- **Code (planned):**
  - `crates/devboy-vault-crypto/` — file format + AEAD + KDF
  - `crates/devboy-secrets-agent/` — daemon, JSON-RPC server,
    socket handling
  - `crates/plugins/secrets/local-vault/` — `SecretSource`
    implementation talking to the daemon
  - `crates/devboy-secret-patterns/` — pattern catalogue
  - `crates/devboy-secrets-ui/` — `ratatui` + `egui` UIs (one
    crate, two backends behind `cfg`)
  - `crates/devboy-cli/` — `devboy secrets {ui, vault, agent,
    rotate, provision, bootstrap, recover, migrate}` subcommands
  - `crates/devboy-mcp/` — `secrets.*` MCP tool surface, the
    "agent never sees value" enforcement, the manifest-gated
    resolver in provider tools
  - `skills/setup-secrets/` — onboarding skill
- **Documentation (planned):**
  - `docs/guide/secrets/onboarding.md` — manual bootstrap flow
  - `docs/guide/secrets/local-vault.md` — file format, recovery,
    backup recommendations
  - `docs/guide/secrets/agent-protocol.md` — the MCP surface,
    aimed at agent authors
  - `docs/guide/secrets/source-plugin-protocol.md` — from
    ADR-021 section 6, cross-referenced

## References

- [ADR-005: Credential storage](./ADR-005-credential-storage.md)
- [ADR-019: Secrets carry SecretString end-to-end](./ADR-019-secret-string-discipline.md)
- [ADR-020: Secret manifest, path convention, and alias resolution](./ADR-020-secret-manifest-and-alias-resolution.md)
- [ADR-021: External secret sources and backend routing](./ADR-021-external-secret-sources.md)
- [`secrecy` crate documentation](https://docs.rs/secrecy/)
- [`chacha20poly1305` crate (RustCrypto)](https://docs.rs/chacha20poly1305/)
- [`argon2` crate (RustCrypto)](https://docs.rs/argon2/)
- [BIP-39: Mnemonic code for generating deterministic keys](https://github.com/bitcoin/bips/blob/master/bip-0039.mediawiki)
- [`ratatui` book](https://ratatui.rs/)
- [`egui` documentation](https://docs.rs/egui/)
- [Model Context Protocol](https://modelcontextprotocol.io/) — wire-format reference for the daemon and agent protocols
- [RFC 8439: ChaCha20 and Poly1305 for IETF Protocols](https://datatracker.ietf.org/doc/html/rfc8439)

---

## Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-05-09 | Andrei Mazniak | Initial draft as the umbrella UX layer above ADR-021 routing — local vault crypto, daemon, native UI, manual-assisted rotation, pattern catalogue, agent provisioning protocol, `setup-secrets` skill |
