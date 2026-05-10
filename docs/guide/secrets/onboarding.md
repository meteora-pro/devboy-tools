# Secret framework onboarding

First-run setup of the secret framework on a new machine or in a new repository. This document walks from "nothing configured" to "`devboy doctor --secrets` is green and the manifest passes CI".

> **When to use**: after `npm install -g @devboy-tools/cli` and the basic `devboy onboard` (see [Quick start](../getting-started/quick-start.md)). If your project already has `.devboy/secrets.toml` and you want an interactive wizard, run `setup-secrets` via the AI agent — it automates the same steps.

> Russian translation: [`ru/onboarding.md`](./ru/onboarding.md).

## Readiness checklist

- [ ] `devboy --version` answers (CLI installed).
- [ ] OS keychain available (macOS Keychain / Windows Credential Manager / Linux Secret Service).
- [ ] Permissions to create `~/.devboy/secrets/` and `<project>/.devboy/`.
- [ ] You know which secrets the project needs (one is enough to start — the manifest grows as you go).

---

## Step 1. Install and verify the CLI

If `devboy` is already on `PATH`, skip this step.

```bash
npm install -g @devboy-tools/cli
devboy --version
```

Alternative via `cargo`:

```bash
cargo install devboy-cli
```

Confirm the secrets subsystem is available:

```bash
devboy secrets --help
```

You should see subcommands `list`, `describe`, `validate`, `migrate`, `agent`, `ui`, `rotate`, `catalog`. If any are missing, upgrade the CLI to 0.26 or later (the minimum version that ships the secret framework).

## Step 2. Wire up the first source

By default the CLI works against the OS keychain with no configuration. That's enough for one developer on one machine. When the team adds 1Password, Vault, or env-store, you extend the router config. Start with keychain.

Create `~/.devboy/secrets/sources.toml` (if you don't have one yet):

```bash
mkdir -p ~/.devboy/secrets
```

```toml
# ~/.devboy/secrets/sources.toml
schema_version = 1

[[source]]
name = "default-keychain"
type = "keychain"
# No credentials needed — the keychain manages access itself.

# When no `[[route]]` matches by prefix, the router falls back
# to the default source. `fallback` engages when the default
# answers `NotInstalled` (CI without an OS keychain).
[default]
source = "default-keychain"
```

Verify the router sees the source:

```bash
devboy doctor --checks context-secrets --format json
```

You should get an entry like `{"name": "default-keychain", "type": "keychain", "available": true}`.

> **If you plan to use 1Password / Vault**: add a separate `[[source]]` block of type `1password` or `vault`, plus a `[[route]]` with the matching `prefix`. Details: [docs/guide/secrets/local-vault.md](./local-vault.md).

## Step 3. Project manifest

The manifest declares which secrets the project expects. Without it the framework has no idea what to look for and `doctor` cannot complain about missing values.

Create `<project>/.devboy/secrets.toml`:

```toml
# <project>/.devboy/secrets.toml
required = [
  "team/<provider>/api-key",
  "team/<provider>/deploy-token",
]

optional = [
  "personal/<provider>/feature-flag-token",
]

[overrides."team/<provider>/api-key"]
description = "Used by the CI pipeline when publishing artefacts."
rotate_every_days = 90

[secret."sandbox/<provider>/local-token"]
description = "Local smoke tests only; never committed."
retrieval_url = "https://example.invalid/<provider>/tokens"
pattern_id = "generic-bearer"
rotation_method = "manual"
```

Structure (see [ADR-020](https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-020-secret-manifest-and-alias-resolution.md)):

- `required` — list of paths in ADR-020 form (`<scope>/<provider>/<purpose>`); the project does not work without their values.
- `optional` — paths that improve UX (mailers, feature flags) but don't block the build.
- `[overrides."<path>"]` — per-path metadata patches over the global index (`~/.devboy/secrets/index.toml`).
- `[secret."<path>"]` — full declaration for a path that exists only in this project (sandbox scenarios, local playgrounds).

> **Use placeholders, not real names**: examples use `<provider>` / `<purpose>` placeholders. In a real manifest write concrete segments (`team/jira/api-key`), but **never** publish client names, internal team codes, or server IDs to GitHub — that's an infra leak. For those, use the `team/`-scope with a generic provider name.

## Step 4. Validate the manifest

Re-run format validation after every manifest edit:

```bash
devboy secrets validate
```

What it checks (see [ADR-021] §6):

- Every path is valid ADR-020 (3+ segments, kebab-case).
- `required` ∩ `optional` is empty.
- `rotate_every_days`, `expires_at`, `pattern_id` are well-typed.
- With the liveness flag (`--liveness`), the framework also checks that for every `required` path there's a router source that can return a value.

Wire the command into a pre-commit hook or CI pipeline:

```yaml
# .github/workflows/secrets.yml
- run: devboy secrets validate --strict
```

`--strict` promotes warnings about non-conformant paths (P10.1) to errors.

## Step 5. Inspect what the framework sees

```bash
devboy secrets list
```

The output is a table with columns `path`, `status`, `expires_at`, `provider`. Metadata only — no values.

Per-path details:

```bash
devboy secrets describe team/<provider>/api-key
```

Shows `description`, `retrieval_url`, `rotation_method`, `last_rotated_at`, `pattern_id`, and every `overrides` patch applied on top of the global index.

JSON mode for scripts:

```bash
devboy secrets list --json | jq '.[] | select(.status == "missing")'
```

## Step 6. Provision values

Values land in the keychain through `secrets ui` (TUI/GUI with a provision dialog) or via the AI agent (`secrets_request_provision` MCP tool). Pick the mode that fits:

- **Interactive (recommended)**:

  ```bash
  devboy secrets ui --tui
  ```

  Opens a TUI with the inventory view. Arrow keys navigate, Enter opens the provision dialog for the selected path, `q` quits. In a GUI environment the CLI auto-detects `$DISPLAY` / `$WAYLAND_DISPLAY` and launches the egui window instead.

- **Via the AI agent**: ask the agent to "provision missing secrets" — it runs the `setup-secrets` skill (see [docs/guide/secrets/agent-protocol.md](./agent-protocol.md)).

- **Scripted** (only for repeatable secrets on one machine):

  ```bash
  devboy secrets rotate team/<provider>/api-key --from-stdin --yes <<<"<value>"
  ```

  Stamps the rotation time and validates the format. For initial provisioning the equivalent is to bring the daemon up and write through the provision flow; see [docs/guide/secrets/local-vault.md](./local-vault.md).

After values are in, `secrets list` shows `status = provisioned` for paths whose value actually landed in the keychain.

## Step 7. Final check

```bash
devboy doctor --secrets
```

A green run means:

- Every `required` path is `provisioned`.
- Every source in `sources.toml` is `available`.
- No non-conformant paths in the keychain (legacy entries migrated — see `devboy secrets migrate`).
- Daemon is running (only if you actually use `secrets ui` or the MCP tools).

If anything is red, `doctor` prints the exact code and suggests a fix command. Next stops: the `repair` skill or the [Doctor](../configuration/doctor.md) section.

## What's next

- [`local-vault.md`](./local-vault.md) — set up your own local store (zeroize-on-drop file with XChaCha20-Poly1305).
- [`token-catalog.md`](./token-catalog.md) — author per-provider procedure files (`kimi.json`, `openai.json`, …) the GUI binds to.
- [`catalog-url-sources.md`](./catalog-url-sources.md) — serve the catalog over the network with sha-pinning + audit log.
- [`agent-protocol.md`](./agent-protocol.md) — how the AI agent works with secrets through MCP without ever seeing the values.
- [`source-plugin-protocol.md`](./source-plugin-protocol.md) — add your own source via the subprocess plugin protocol.
- ADR-020 / ADR-021 / ADR-023 — the formal specs of the manifest, the router, and the UX layer.

[ADR-021]: https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-021-secret-source-router.md
