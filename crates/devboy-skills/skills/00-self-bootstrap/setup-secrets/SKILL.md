---
name: setup-secrets
description: Walk a project from "no values provisioned" to "doctor --secrets is green" — eight idempotent steps with resume support via setup-state.toml. Wraps the secret framework (ADR-023 §3.8) for AI agents and headless onboarding.
category: self-bootstrap
version: 1
compatibility: devboy-tools >= 0.27
activation:
  - "setup secrets"
  - "provision secrets"
  - "bootstrap secrets"
  - "missing secrets"
  - "configure secret framework"
tools:
  - secrets
  - doctor
  - config
---

# setup-secrets

Walk a project's secret framework from `[]` to "every required path is provisioned and `devboy doctor --secrets` is green". The procedure is **eight idempotent steps**: each step records its outcome in `~/.devboy/secrets/setup-state.toml`; a re-run resumes from the first incomplete step rather than restarting at step 1.

The skill never sees secret values directly. Provisioning happens through `secrets_request_provision` (the user types into the dialog → the daemon stores the value → the agent only sees `status: ok`). See [`agent-protocol.md`](https://github.com/meteora-pro/devboy-tools/blob/main/docs/guide/secrets/agent-protocol.md) for the trust boundary.

## When to use

- The project has `<repo>/.devboy/secrets.toml` with `required = [...]` paths and `devboy secrets list --json` reports any of them as `missing`.
- The user typed "setup secrets" / "provision missing secrets" / "bootstrap secret framework".
- The `setup` skill (00-self-bootstrap/setup) detected a manifest with required paths and delegated to this skill.
- A re-run after a prior partial setup — the wizard picks up where it left off.

For a *broken* setup (corrupt vault, daemon down) use `repair` instead. For *creating* the manifest itself use `00-self-bootstrap/setup` first; this skill assumes the manifest exists.

## Procedure

The skill drives an **eight-step wizard**. After each step, emit one structured message to the agent of shape `{step, status, summary, next_options}` and **wait for the user** before continuing — `next` / `skip` / `abort`.

### 0. Initialise state file

Run the entry helper at `entry.sh` (or `devboy secrets bootstrap --state-file ~/.devboy/secrets/setup-state.toml` if the CLI is on `PATH`). The helper creates the state file if missing and prints the first-incomplete step, so a fresh run starts at step 1 and a resumed run jumps straight to where the previous attempt stopped.

```bash
bash "$DEVBOY_SKILL_DIR/setup-secrets/entry.sh"
```

State schema (TOML, P26.5 spec):

```toml
schema_version = 1
started_at = "2026-05-10T17:30:00Z"
last_step = 0
[steps.vault_state]      = { status = "pending" }
[steps.create_vault]     = { status = "pending" }
[steps.touch_id]         = { status = "pending" }
[steps.routing]          = { status = "pending" }
[steps.required]         = { status = "pending" }
[steps.optional]         = { status = "pending" }
[steps.validation]       = { status = "pending" }
[steps.doctor]           = { status = "pending" }
```

Statuses: `pending` / `in-progress` / `done` / `skipped` / `failed`. The wizard advances `last_step` and re-writes the file at the end of each step.

### 1. Vault state

Check whether `~/.devboy/secrets/local-vault.dvb` exists and whether the OS keychain is available. Two branches:

- **Vault present** OR **keychain available**: mark `vault_state = done` and jump to step 4. The user does not need to create a local vault.
- **Neither**: mark `vault_state = done`, proceed to step 2.

Probe via `devboy doctor --checks context-secrets --format json | jq '.findings[] | select(.id == "vault" or .id == "keychain")'`.

### 2. Create vault

Only runs when step 1 routed here.

- Prompt for a passphrase (min 12 chars). Confirm twice — if the two entries differ, repeat until they match.
- Generate a 24-word recovery phrase. Display it once with explicit acknowledgement (`I have written this phrase down: yes/no`) and **do not store it**.
- Write `local-vault.dvb` with the passphrase + recovery envelopes.

Mark `create_vault = done` and continue.

### 3. Optional Touch ID

macOS only. Ask: `Add a Touch-ID unlock for the vault? (y/N)`.

- `y` — call `devboy secrets vault add-envelope --touchid` and mark `touch_id = done`.
- `n` (or non-macOS) — mark `touch_id = skipped`.

### 4. Configure routing

Walk the candidate sources (`keychain`, `local-vault`, `1password`, `vault`, `env`). For each:

- **Available** — register it in `~/.devboy/secrets/sources.toml`.
- **Unavailable** (e.g. `op` not installed) — record a `skipped` status with a one-line install hint (`brew install 1password-cli`).

Always set `keychain` (or `local-vault` on Linux) as `[default]`. Mark `routing = done`.

### 5. Walk required secrets

For each path in `<repo>/.devboy/secrets.toml`'s `required = [...]` list:

```bash
devboy secrets describe "$path" --json
# -> { description, retrieval_url, capabilities_hint, ... }
```

- If already provisioned and `expires_at` is more than 14 days away — skip silently, record `done`.
- Otherwise call `secrets_request_provision({path})` (MCP) or `devboy secrets ui` (interactive). The user pastes the value into the dialog. Poll status; record `done` on `ok`, `failed { reason }` on `cancelled` / `expired` / `failed`.

Mark `required = done` once every path is settled.

### 6. Walk optional secrets

Same flow as step 5, run over `optional = [...]`. Missing values do **not** mark the step `failed` — they map to `skipped` with the path listed in the summary.

### 7. Validation

Run `devboy secrets validate --strict --liveness`. The check enforces ADR-020 path discipline plus a router-side probe that every required path resolves. On failure, the wizard loops back to the offending path; on success, mark `validation = done`.

### 8. Doctor

Run `devboy doctor --secrets`. Expected to pass after steps 1-7. On failure, the wizard reads the error code, maps it to the relevant step, and loops back. On success, mark `doctor = done` and emit a final `wizard_complete` message.

## Inputs and outputs

The skill operates on the manifest files already in place — it does not edit them. Every value lands through the dialog, never through the agent.

Reads:
- `<repo>/.devboy/secrets.toml`
- `~/.devboy/secrets/index.toml`
- `~/.devboy/secrets/sources.toml`

Writes (only metadata; never raw secret values):
- `~/.devboy/secrets/setup-state.toml`
- `~/.devboy/secrets/sources.toml` (during step 4)
- `~/.devboy/secrets/local-vault.dvb` (during step 2, opaque to the skill)

## Resume semantics

A re-run reads `setup-state.toml` and starts at the first step whose status is not `done` / `skipped`. The wizard never re-prompts for completed steps; this is what makes the skill safe to invoke from cron or from the `setup` skill's "secrets bootstrap" hook.

To force a fresh run, the user deletes the state file:

```bash
rm ~/.devboy/secrets/setup-state.toml
```

## Failure handling

The eighth step is the only assertion of overall success. Any earlier step that returns `failed` halts the wizard with a structured message indicating the step, the path (if applicable), and the recommended next action. The agent surfaces the message and waits.

## Out of scope

- Editing the manifest itself. The user authors `<repo>/.devboy/secrets.toml`; this skill does not propose new paths. For that, see `secrets_propose_new_path` (P24 phase).
- Rotating already-provisioned values. The wizard skips paths that are not expiring; rotation is a separate flow (`devboy secrets rotate <path>`).
- Approve-on-use policy. The skill provisions values; the `approve_on_use` field affects *use*, not provisioning. See ADR-023 §3.7.

## See also

- [ADR-023](https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-023-secret-store-ux-layer.md) §3.8 — formal spec of the eight-step flow.
- [`onboarding.md`](https://github.com/meteora-pro/devboy-tools/blob/main/docs/guide/secrets/onboarding.md) — manual equivalent for users without an agent.
- [`agent-protocol.md`](https://github.com/meteora-pro/devboy-tools/blob/main/docs/guide/secrets/agent-protocol.md) — MCP tool surface used in steps 5-6.
- `crates/devboy-skills/skills/00-self-bootstrap/setup/SKILL.md` — the parent bootstrap skill.
