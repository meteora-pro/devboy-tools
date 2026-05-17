# `setup-secrets` skill

The AI-driven sibling of [`onboarding.md`](./onboarding.md). When a project has a manifest but no provisioned values yet, the user can ask the agent to "set up secrets" and the agent runs the eight-step `setup-secrets` skill instead of walking the user through the bash commands by hand.

This page covers what the skill does, the on-disk state file, and the resume contract that lets a re-run pick up exactly where the previous attempt stopped.

> Russian translation: pending — will land alongside the broader Russian docs sweep.

---

## When to use

- A project has `<repo>/.devboy/secrets.toml` with `required = [...]` paths and `devboy secrets list --json` reports any of them as `missing`.
- The user asked the agent to "provision missing secrets" / "bootstrap secret framework" / "set up secrets".
- The `setup` skill (`crates/devboy-skills/skills/00-self-bootstrap/setup/`) detected a manifest with required paths and delegated.
- A re-run after a partial setup — the wizard skips already-`done` phases automatically.

For a *broken* setup (corrupt vault, daemon down) use the `repair` skill instead. For *creating* the manifest itself the user runs `setup` first; this skill assumes the manifest exists.

---

## Eight-step flow

The skill is **idempotent**: every step records its outcome in `~/.devboy/secrets/setup-state.toml`. A re-run reads the file and resumes from the first step whose status is not `done` / `skipped`.

| # | Step | Status keys it touches | Notes |
|---|------|------------------------|-------|
| 1 | Vault state | `vault_state` | Probe `local-vault.dvb` + OS keychain. Skip 2-3 if either is available. |
| 2 | Create vault | `create_vault` | Passphrase + 24-word recovery phrase. Recovery phrase shown once and not stored. |
| 3 | Optional Touch ID | `touch_id` | macOS only; `skipped` elsewhere. |
| 4 | Configure routing | `routing` | Walk candidate sources (`keychain`, `local-vault`, `1password`, `vault`, `env`); register or note as skipped. |
| 5 | Walk required secrets | `required` (= `provision`) | For each path in `required = [...]`: `secrets_describe` → if missing, open the provision dialog, await the user. |
| 6 | Walk optional secrets | `optional` | Same shape as step 5; missing values map to `skipped`, not `failed`. |
| 7 | Validation | `validation` | `devboy secrets validate --strict --liveness` — format + router probe. |
| 8 | Doctor | `doctor` | `devboy doctor --secrets` is the assertion of overall success. |

Each step emits one structured message of shape `{step, status, summary, next_options}` and **waits for the user** before continuing (`next` / `skip` / `abort`). The skill never advances on its own.

The CLI runner exposed by `crates/devboy-cli/src/secrets_setup.rs` covers a slimmer four-phase subset — `Scan / Propose / Provision / Doctor` — which is the credentials-only path most projects need. The eight-step flow is the broader procedure that includes vault bootstrapping; the four-phase runner maps onto steps 5 and 8 directly and is what most callers integrate with first.

---

## State file

Path: `~/.devboy/secrets/setup-state.toml` (override with `$DEVBOY_SECRETS_HOME`).

Schema:

```toml
schema_version = 1
started_at = "2026-05-10T17:30:00Z"
last_step = 4

[steps.vault_state]
status = "done"
completed_at = "2026-05-10T17:30:01Z"

[steps.create_vault]
status = "skipped"
note = "keychain available — vault not needed"

[steps.touch_id]
status = "skipped"
note = "non-macOS host"

[steps.routing]
status = "done"
note = "default=keychain; 1password=skipped (op not installed)"

[steps.required]
status = "in-progress"

[steps.optional]
status = "pending"

[steps.validation]
status = "pending"

[steps.doctor]
status = "pending"

# The four-phase runner adds these alongside the eight-step keys.
[steps.scan]
status = "done"
note = "hits=12"

[steps.propose]
status = "done"
note = "paths=8, skipped=4"

[steps.provision]
status = "done"
note = "provisioned=2: team/jira/api-key, team/openai/api-key"
```

Status values:

- `pending` — step has not run yet.
- `in-progress` — step started, has not settled. Only present mid-run.
- `done` — step completed successfully.
- `skipped` — step deemed not applicable (`Touch ID` on Linux, `optional` paths the user skipped, `provision` when nothing was missing).
- `failed` — step encountered an error the wizard could not recover from. The next run picks up from this step.

`last_step` advances monotonically as the wizard reaches each phase, so a status renderer can show "you got to step 4 of 8" without iterating the table.

---

## Resume contract

The skill — and the four-phase CLI runner — never *re-do* a step that has already settled. The contract:

- A step with status `done` or `skipped` is treated as final. The wizard does not re-prompt, re-scan, or re-provision; it emits one `Skipped` event with reason `already done` and moves on.
- A step with status `failed` is treated as not yet settled. The next run re-attempts it. This is the recovery path: the user fixes whatever caused the failure (kills the dialog, restarts the daemon, edits the manifest) and re-runs the same command.
- A step with status `pending` or `in-progress` is also treated as not yet settled. `in-progress` only appears mid-step; the wizard never advances `last_step` to "completed" without flushing a terminal status.
- The state file is written after every settled phase, so an interrupted wizard mid-phase still leaves a coherent file.

To force a fresh run, delete the state file:

```bash
rm ~/.devboy/secrets/setup-state.toml
```

The `entry.sh` helper (P26.1) re-creates a fresh state file with every step pending. Resume is opt-in by not deleting the file.

---

## Typed view

The `devboy-cli::secrets_setup::SetupState` struct is the typed projection of `setup-state.toml` for in-process callers — the future `devboy secrets setup --resume` sub-command, doctor checks, status renders. Shape:

```rust
pub struct SetupState {
    pub scanned: bool,
    pub proposed: bool,
    pub provisioned: Vec<String>, // ADR-020 paths the wizard saved
    pub last_step: u8,
}

impl SetupState {
    pub fn is_complete(&self) -> bool;
}

pub fn read_setup_state(path: &Path) -> SetupState;
```

A missing or malformed file maps to a fresh `SetupState` (`scanned=false`, `last_step=0`); the wizard treats both the same way, so callers don't have to special-case "first run".

---

## Failure handling

The eighth step is the only assertion of overall success. Any earlier step that returns `failed` halts the wizard with a structured message indicating the step, the path (when applicable), and the recommended next action. The agent surfaces the message and waits.

Common failure modes:

- **`vault_state` failed** — keychain probe panicked. User fixes the keychain (re-login, unlock); the wizard re-probes on the next run.
- **`create_vault` failed** — disk full / permissions. State file records the reason; the user resolves and re-runs.
- **`required` / `provision` failed** — the user cancelled the dialog, the dialog timed out, or the daemon rejected the value. The wizard records the path that failed and stops; the user retries the same path on the next run.
- **`doctor` failed** — pretty rare after steps 1-7 settle, but it does happen if `validation` skipped a transient probe failure. The wizard reads the doctor error code and points the user at the failing path.

---

## Provision dialog: what the user sees

When the wizard reaches step 5 (provision a missing path), it opens the native provision dialog (`devboy secrets ui --provision <path>`). The dialog now fuses three sources of metadata so the user sees the same procedure the provider's own docs publish:

The dialog opens as a **modal overlay** on top of the inventory (egui) / a centered modal in the terminal (TUI) — the inventory stays visible underneath. ESC or a click on the dimmed backdrop dismisses it. Content is ordered most-actionable-first.

| Row / block | Mode | Source | Notes |
|---|---|---|---|
| heading | both | — | "Provision secret" / "Rotate secret". |
| PATH | both | manifest | The ADR-020 path. |
| VIA | both | router resolution | Source name (`local-vault`, `keychain`, `1password`, …). |
| ROTATION | **Rotation only** | catalog → manifest → `"manual"` | Cadence row. Omitted in Provision mode — it's noise when first provisioning. |
| FORMAT | both | catalog | `variants[i].format_hint` (e.g. "starts with sk-, 20+ chars"). |
| Description block | both | catalog | Italic text — `variants[i].description`. Hidden when no catalog match. |
| Links row | both | catalog → manifest | "Open console ↗" (`retrieval.console_url`, falls back to `IndexEntry.retrieval_url`) + "Provider docs ↗" (`retrieval.docs_url`). Both are **real hyperlinks** — egui opens the OS browser directly. Placed *above* the steps. In the TUI these are `CONSOLE` / `DOCS` text lines. |
| "How to obtain:" | both | catalog | Numbered list rendered from `variants[i].retrieval.steps`. |
| "Note: …" | both | catalog | Caveat / pro-tip from `variants[i].retrieval.notes`. |
| "Rotating this secret:" | **Rotation only** | catalog | `variants[i].rotation.notes` (the concrete *how*) + a "Rotation guide ↗" link from `variants[i].rotation.guide_url`. Omitted in Provision mode — rotation guidance belongs on the rotation flow, not the first-time provision. Hidden in Rotation mode too when neither field is set. |
| value input | both | — | Password-masked input — the actual action, below a separator. An eye-toggle (👁 / 🙈) right of the field unmasks it **in place** (standard password-field UX); a `shown / hidden · N chars` caption sits below. No separate "reveal" checkbox. |
| destructive-confirm | **Rotation only** | — | "I understand this overwrites the current secret" checkbox gates the save. |

When the path's provider segment does not match any loaded catalog the catalog-derived blocks collapse silently and the dialog renders only the manifest-side rows — the pre-S2 behaviour.

The variant chip in the inventory row (when the catalog declares more than one variant) re-arms the dialog with the selected variant's metadata; nothing else in the user's input is affected.

The builder lives in `crates/devboy-secrets-ui-bin/src/catalog_metadata.rs::metadata_from_catalog_and_entry` and is exercised by `cargo test -p devboy-secrets-ui-bin catalog_metadata`.

## Choosing a backend

Where the provision dialog's *Save* writes depends on the backend the UI resolved at startup:

| Backend | Reached when | Provision dialog writes to |
|---|---|---|
| OS keychain | default — no `DEVBOY_VAULT_PASSPHRASE`, no `.dvb` file | the OS keychain (service `devboy-tools`) |
| local-vault (unlocked) | `DEVBOY_VAULT_PASSPHRASE` set, OR the user unlocked / created the vault in the UI | the encrypted `.dvb` file (XChaCha20-Poly1305 + Argon2id) |
| local-vault (locked) | a `.dvb` file exists but no env passphrase | nothing — the UI shows a modal **unlock prompt** before the inventory is usable |
| HashiCorp Cloud Vault | a `type = "vault"` `[[source]]` is configured (via the onboarding wizard) | **read-only today** — the UI reads provisioned status from Vault but refuses writes with an honest message (the `SecretSource` write surface ships in P15) |

The UI handles the locked → unlocked transition itself: a modal asks for the passphrase (eye-toggle, wrong-passphrase shown in red, *Use keychain instead* escape hatch), and a top-bar switcher flips backends live. First run with no vault file offers a create flow with a one-time recovery-phrase gate.

**First-run onboarding.** When nothing is configured anywhere (no `sources.toml`, no `.dvb`, no env passphrase), the UI opens an onboarding wizard: a backend picker for keychain / local-vault / HCP Vault, combinations allowed, one marked primary. It writes `sources.toml` and creates the local vault if asked. The HCP Vault entry carries an `access = "read" | "readwrite"` mask — `read` narrows the source's capabilities to `READ | LIST | VALIDATE`.

Full walkthrough: [`local-vault.md`](./local-vault.md) ("Unlocking from the secrets UI", "First-run onboarding wizard", "External Vault as a read-source"). Contracts: [`scenarios/vault-unlock.feature`](./scenarios/vault-unlock.feature), [`scenarios/onboarding.feature`](./scenarios/onboarding.feature).

## See also

- [ADR-023](https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-023-secret-store-ux-layer.md) §3.4 — provision dialog contract.
- [ADR-023](https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-023-secret-store-ux-layer.md) §3.8 — formal spec of the eight-step flow.
- [`onboarding.md`](./onboarding.md) — manual equivalent for users without an agent.
- [`agent-protocol.md`](./agent-protocol.md) — MCP tool surface used in steps 5-6 (`secrets_request_provision`, `secrets_poll_status`).
- [`scenarios/ui-catalog-rendering.feature`](./scenarios/ui-catalog-rendering.feature) — Gherkin contract for the provision dialog's catalog binding.
- [`scenarios/vault-unlock.feature`](./scenarios/vault-unlock.feature) — Gherkin contract for the local-vault unlock / create flow.
- [`local-vault.md`](./local-vault.md) — local-vault file format, the UI unlock paths, recovery, and backup.
- `crates/devboy-skills/skills/00-self-bootstrap/setup-secrets/SKILL.md` — the skill manifest and procedure walkthrough the agent loads at activation time.
- `crates/devboy-cli/src/secrets_setup.rs` — the CLI-side scanner / proposer / wizard runner, including `read_setup_state`.
