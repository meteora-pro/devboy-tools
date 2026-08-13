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

## For operators: short-lived setup commands

*Skip this section unless you run a config server that hands people a `devboy init` command to paste.*

A setup command carries a token, and every copy of it — shell history, chat, a screen share, a support ticket — is a live credential for as long as that token is valid. Devboy can trade the pasted token for a durable one at install time, so what got copied is worthless within minutes.

Opt in by adding one field to the TOML your config endpoint already serves:

```toml
[remote_config]
token_exchange_url = "https://config.example.com/api/config/exchange"
```

`devboy init` then makes one request:

```text
POST <token_exchange_url>
Authorization: Bearer <the pasted token>
Accept: application/json

→ 200 {"token": "<durable token>", "expires_at": "2027-01-01T00:00:00Z"}
```

`expires_at` is optional and advisory — it is shown to the user, not enforced.

Three rules the client applies, so you can design the server around them:

- **The exchange URL must share an origin with the config URL.** Acting on a URL that arrived in a response means posting a live credential to wherever it points; devboy will only do that against the host the user already chose to trust. Host your exchange endpoint on the same origin, or do not declare one.
- **The client exchanges once and does not retry.** A bootstrap token is expected to be consumed by the first successful exchange. Whether it is truly single-use, and what a replay is recorded as, is yours to decide — the client will not paper over a failure by trying again.
- **A failed exchange fails `init`.** Storing a token that dies in minutes would produce an install that looks finished and stops working. The user is told to ask for a fresh setup command while the old one is still on screen.

Declaring nothing keeps the old behaviour exactly: the token supplied on the command line is stored as-is. Nothing here is required to use devboy.

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

### Companion binaries

The npm package and the GitHub Release tarball ship two companion binaries alongside `devboy`:

- **`devboy-secrets-agent`** — long-running daemon that owns the unlocked local-vault. Spawned on demand by `devboy secrets agent start`; not required unless you use the `local-vault` source.
- **`devboy-secrets-ui`** — native GUI window (eframe / egui) for the inventory + provision dialog. Spawned by `devboy secrets ui --gui` as a subprocess; not required for TUI / CI-only usage.

Both are placed next to `devboy` in the same `bin/` directory, so the CLI's discovery walk (env override → sibling of `current_exe()` → `PATH`) finds them out of the box. If you build from source via `cargo install --path crates/devboy-cli`, run the same `cargo install` against `crates/devboy-secrets-agent` / `crates/devboy-secrets-ui-bin` to get the companions. The `DEVBOY_AGENT_BIN` and `DEVBOY_UI_BIN` env vars override discovery for tests / dev workflows.

The split keeps the CLI itself lean (~19 MiB) — CI runs hitting `secrets list` / `secrets validate` / the MCP server never link the eframe rendering stack.

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

### Using an existing KeePass database

If you already keep your tokens in a KeePass `.kdbx` file, point devboy at it via env var (no router config needed for read-only use):

```bash
export DEVBOY_KDBX_FILE=~/Documents/secrets.kdbx
# Optional companion keyfile (two-factor unlock):
# export DEVBOY_KDBX_KEYFILE=~/Documents/secrets.keyx

devboy secrets ui --gui
```

The UI window opens with an "Unlock KeePass database" modal asking for the passphrase. After unlock the inventory populates with one row per KeePass entry. Path mapping: `Personal / Cloud / "AWS Access Key"` → `kdbx/personal/cloud/aws-access-key`. The decrypted snapshot lives inside the UI process only — the `devboy-secrets-agent` daemon never opens the file. Read-only MVP; writing back lands as a follow-up.

For a non-GUI smoke (CI / headless dev box):

```bash
devboy secrets kdbx peek --file ~/Documents/secrets.kdbx
# Prompts for passphrase in-terminal (no echo); prints the inventory
# table — path / Title / UserName / URL / `password?` yes-no. Values
# are never printed.
```

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
# Force agents to confirm every USE of this token, not just its
# provision. See "Step 4b. Approve-on-use policy" below.
approve_on_use = "per-call"

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

## Step 4b. Approve-on-use policy (optional)

Most paths resolve silently — the proxy alias resolver swaps `@secret:<path>` for the value with no agent prompt. For high-stakes credentials (production database password, signing key, billing API token), the manifest can require the *user* to confirm each *use* even after the value has been provisioned. This is **separate** from rotation gating: rotation already requires a destructive-confirm dialog; approve-on-use covers reads.

Three policies live on the `approve_on_use` field of an `IndexEntry` or `OverrideEntry`:

| Policy | Behaviour | Right for |
|---|---|---|
| `never` (default) | Resolve silently — no dialog. | Most paths. |
| `session` | Prompt on first resolve in the process; cache the approval for the rest of the session. | Stage credentials you don't want to log every five minutes but still want logged once. |
| `per-call` | Prompt on every resolve; cache is bypassed. | Production credentials, signing keys, anything destructive. |

Examples:

```toml
# ~/.devboy/secrets/index.toml — global default for a path
[secret."team/prod-db/password"]
description = "Production DB password — approve every use."
rotation_method = "manual"
approve_on_use = "per-call"

# <project>/.devboy/secrets.toml — project-level tightening
[overrides."team/<provider>/api-key"]
approve_on_use = "session"
```

Override precedence per [ADR-020] §4: `[overrides."<path>"].approve_on_use` wins over the global index entry. The project can therefore tighten (move from `never` to `per-call`) without rewriting the global index. Loosening (`per-call` → `never`) works the same way and is the contract — the project owner has the authority to relax their own copy.

What the dialog looks like is in [`agent-protocol.md`](./agent-protocol.md#secrets_request_use_approval). The cache that holds `session` approvals is `devboy-core::secret_approval::SessionApprovalCache`, scoped per-process; closing the agent session clears it.

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
- [`setup-skill.md`](./setup-skill.md) — the AI-driven equivalent of this page: eight-step flow, `setup-state.toml` shape, resume contract.
- [`source-plugin-protocol.md`](./source-plugin-protocol.md) — add your own source via the subprocess plugin protocol.
- ADR-020 / ADR-021 / ADR-023 — the formal specs of the manifest, the router, and the UX layer.

[ADR-021]: https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-021-secret-source-router.md
