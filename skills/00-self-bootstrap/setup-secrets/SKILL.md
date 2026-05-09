---
name: setup-secrets
description: First-run wizard for the devboy secret framework — walk a fresh project from "no secret manifest, no router, no daemon" to "every required secret provisioned and verified". Idempotent eight-step flow per ADR-023 §3.8 with state at ~/.devboy/secrets/setup-state.toml so the user can resume or skip.
category: self-bootstrap
version: 1
compatibility: devboy-tools >= 0.26
activation:
  - "setup secrets"
  - "configure devboy secrets"
  - "set up the secret framework"
  - "wire up devboy secrets"
  - "onboard secrets"
tools:
  - secrets_list
  - secrets_describe
  - secrets_request_provision
  - secrets_poll_status
  - doctor
  - secrets
---

# setup-secrets

Bring a project from a cold-start "no manifest, no router, no daemon" state to a steady-state "every required secret provisioned and verified". Each step is **idempotent** — re-running the skill resumes from where the user left off rather than starting over.

Per ADR-023 §3.8, the skill is the AI-agent–driven counterpart to `devboy secrets ui`: where the UI lets a human click through the four MVP views (P11.1–P11.4), this skill drives the same workflow with structured messages so the agent can pace itself with the user.

## When to use

- The project has no `.devboy/secrets/` directory and `devboy doctor --secrets` reports unconfigured paths.
- The user just installed the secrets framework and asked the agent to "set things up" / "configure secrets".
- The agent needs to provision a handful of new credentials and wants a guided flow rather than ad-hoc `secrets_request_provision` calls.
- A previous setup-secrets run was interrupted (network drop, agent restart) and the user wants to resume.

For day-to-day rotations of an *already-configured* secret, use `devboy secrets rotate <path>` directly — this skill is for first-run / batch setup.

## Idempotency contract

Every step writes its outcome to `~/.devboy/secrets/setup-state.toml`. The next time the skill is invoked, each step inspects its row and either:

- **Skips** when `status = "done"` or `status = "skipped-by-user"`.
- **Re-runs** when `status = "pending"` / `"failed"` / missing.

The user can flip a step back to `pending` by editing the state file; that's the supported way to force a re-run.

## State file schema

```toml
# ~/.devboy/secrets/setup-state.toml
schema_version = 1
started_at = "2026-05-09T20:00:00Z"
last_updated_at = "2026-05-09T20:42:13Z"

[steps.detect_environment]
status = "done"        # one of: pending | done | skipped-by-user | failed
summary = "macOS, agent socket reachable, 4 patterns from catalogue"
next_options = []      # populated when status = pending

[steps.load_manifest]
status = "done"
summary = "merged manifest: 7 paths, 3 required, 4 optional"

[steps.audit_with_doctor]
status = "done"
summary = "doctor reports: 4 missing, 1 expiring, 0 format-invalid"

[steps.configure_router]
status = "skipped-by-user"
summary = "user opted to keep default keychain-only routing"

[steps.discovery_import]
status = "done"
summary = "imported 2 paths from 1Password, 1 from keychain"

[steps.provision_missing]
status = "in-progress"
summary = "provisioned team/jira/api-key; team/openai/api-key still pending"

[steps.verify_all]
status = "pending"

[steps.persist_completion]
status = "pending"
```

## Structured-message contract

Every step emits exactly one message to the user before waiting on input:

```json
{
  "step": "audit_with_doctor",
  "status": "done",
  "summary": "doctor reports: 4 missing secrets, 1 expiring, 0 format-invalid",
  "next_options": ["next", "skip", "abort"]
}
```

`next_options` always includes `next` (advance to the next step) and `abort` (stop the wizard; state file is preserved). `skip` appears when skipping is meaningful — e.g. "configure router" can be skipped if the user is happy with defaults; "provision missing" cannot.

The user replies with one of the offered options. The agent interprets:

- `next` → mark this step `done` (or carry over `in-progress` if the work isn't finished yet), advance.
- `skip` → mark `skipped-by-user`, advance.
- `abort` → stop the wizard, leave state as-is for next run.

Any other reply is treated as "stay on this step" — the agent re-issues the same structured message after handling whatever the user asked.

## Procedure

### Step 1 — Detect environment

```bash
devboy doctor --json --secrets
```

Parse the JSON output. Capture: OS, agent-socket reachability, number of patterns the catalogue offers, current router config (if any).

Update state:

```toml
[steps.detect_environment]
status = "done"
summary = "<os>, agent socket <reachable|unreachable>, <N> patterns, <M> sources configured"
```

Emit the structured message and wait.

### Step 2 — Load manifest

```bash
devboy secrets list --json
```

This merges the global index with the project's `.devboy/secrets.toml` per ADR-020. Capture the count of required vs optional paths.

Update state with `summary = "merged manifest: <T> paths, <R> required, <O> optional"` and emit. If the count is `0` (no manifest yet), this step's summary should call that out plainly — the user may want to abort and add some required paths first.

### Step 3 — Audit with doctor

```bash
devboy doctor --json --secrets
```

Re-run doctor with the loaded manifest and inventory the diagnostics:

- `missing` — paths in the manifest with no provisioned value
- `expiring` — paths whose `expires_at` is within 14 days
- `format-invalid` — paths whose value doesn't match `format_regex` / `pattern_id`
- `non-conformant-paths` — legacy keychain entries flagged by P10.1

Summarise as `"doctor reports: <missing>m, <expiring>e, <format-invalid>fi, <non-conformant>nc"`.

If `non-conformant > 0`, surface a `next_option` named `"migrate"` that runs `devboy secrets migrate` (P10.2) before continuing.

### Step 4 — Configure router

If the user has no `~/.devboy/secrets/sources.toml`, ask whether they want:

- the **default keychain-only routing** (zero config — fits 80% of single-developer setups), or
- a **multi-source router** (1Password / Vault / env-store / local-vault — P5.2).

If they pick the default, write a minimal `sources.toml` with a single keychain source and mark `status = "done"`. If they pick a multi-source setup, walk through `secrets_describe` per source kind, ask for the credentials each source needs (keychain item paths, Vault address, etc.), and write the corresponding `[[source]]` blocks.

The user can `skip` this step if they don't want any change to routing. Mark `status = "skipped-by-user"`.

### Step 5 — Discovery import

For each source the user configured in step 4, run discovery:

```bash
devboy secrets ui --tui --view discovery
```

Or, if running headless, drive the same flow programmatically via `secrets_propose_new_path` (P14.4) for each item the discovery sweep returns. The structured message reports `imported <N> paths from <source>`.

Skippable when the user wants to register paths manually (uncommon — most flows benefit from discovery).

### Step 6 — Provision missing

For every path the doctor diagnostics surfaced as `missing`:

1. Call `secrets_request_provision { path }` (P14.2). Receive `request_id`.
2. Tell the user: "I've opened the dialog for `<path>`. Paste the value when ready, or click Cancel."
3. Poll `secrets_poll_status { request_id }` until `status` is `ok` / `cancelled` / `expired` / `failed`.
4. On `cancelled`: ask whether to skip this path or retry. On `failed`: surface the reason and retry.
5. On `ok`: log progress and continue to the next missing path.

This step can take several user-driven minutes. Update the `summary` after each path so the user can see progress: `"provisioned <K>/<N> required secrets"`.

When all missing paths are resolved (or skipped), mark `done` and advance.

### Step 7 — Verify all

Re-run `devboy doctor --json --secrets` and check that:

- `missing` is now `0` (or every remaining missing path was explicitly skipped).
- `format-invalid` is `0` for required paths.
- The agent socket reports `unlocked`.

If verification fails, drop back to step 6 with the new diagnostic set instead of progressing.

### Step 8 — Persist completion

Mark `migration_complete = true` in `~/.devboy/secrets/config.toml` (P10.3). Write a final `last_updated_at` to the state file. Emit a closing structured message:

```json
{
  "step": "persist_completion",
  "status": "done",
  "summary": "Setup complete. <K> secrets provisioned, <S> skipped. Run `devboy doctor` anytime to re-check.",
  "next_options": []
}
```

`next_options` is empty — the wizard is finished.

## Resuming a previous run

On invocation, the skill's first action is to load `~/.devboy/secrets/setup-state.toml` (when present) and report which steps are already complete. Example opener:

> "Resuming from a previous setup-secrets run. Steps 1–4 are done; step 5 (discovery import) is in-progress with `summary = imported 2 paths from 1Password, 1 from keychain still pending`. Continue from step 5?"

The agent waits for `next` / `restart` / `abort`:

- `next` — pick up at the first non-done step.
- `restart` — wipe the state file (after confirming) and start at step 1.
- `abort` — leave the state file untouched and exit.

## Failure modes

- **Agent socket down**: step 6's `secrets_request_provision` returns the `failed` status with a "no UI launcher" message (the daemon-backed launcher hasn't been wired in this build, or the daemon isn't running). The skill prompts the user to start the daemon (`devboy secrets agent start`) and re-runs the step.
- **Network drop mid-discovery**: step 5 marks the failing source's row in the state with `status = "failed"` and the offending source name. On resume, the skill retries that source.
- **User aborts**: every step has `abort` in `next_options`. The state file is preserved so the next run resumes cleanly.

## Success criteria

- The state file at `~/.devboy/secrets/setup-state.toml` exists with all eight rows present.
- All `status` fields are `done` or `skipped-by-user`.
- `devboy doctor --secrets` exits zero on a final pass.
- The user has a clear summary of what was provisioned vs skipped.

## What this skill does **not** do

- **Rotate already-provisioned secrets.** Use `devboy secrets rotate <path>` (P13.1).
- **Author new secret patterns.** That's a `devboy-secret-patterns` user-extension authoring task — outside the wizard's scope.
- **Touch upstream provider state.** The skill never calls a provider API directly; everything goes through the framework's manifest + the user-facing dialogs.

Per ADR-023 §3.8 (skill-driven setup wizard).
