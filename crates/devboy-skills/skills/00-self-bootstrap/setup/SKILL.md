---
name: setup
description: Bootstrap devboy from scratch — install the CLI if missing, register the MCP server, run `devboy onboard` for the active agent, optionally bootstrap the secret framework, verify with `doctor`. First-run skill for both manual installs and the Claude Code / Codex plugin.
category: self-bootstrap
version: 3
compatibility: devboy-tools >= 0.26
activation:
  - "setup devboy"
  - "configure devboy"
  - "initialise devboy"
  - "install devboy"
  - "bootstrap devboy"
tools:
  - init
  - onboard
  - config
  - doctor
  - test
  - secrets
---

# setup

Bring `devboy` from "nothing on this machine" to "MCP server registered, providers configured, doctor green". Works in two modes:

- **Plugin-driven** — when invoked from the Claude Code plugin (`devboy@meteora-devboy`) or the Codex plugin. The plugin manifest is already loaded; this skill installs the CLI binary and runs `devboy onboard` to wire up the rest. Detected by `$CLAUDE_PLUGIN_DATA` (Claude) or `$CODEX_PLUGIN_DATA` (Codex) being set.
- **Manual** — when the user invoked the skill directly after `npm install -g @devboy-tools/cli`. The CLI is already present; only configuration remains.

For a broken state that needs triage, use `repair` instead.

## When to use

- A new clone of a project and `.devboy.toml` is missing.
- A new machine and `devboy doctor` reports no configured providers.
- The Claude Code or Codex plugin was just installed and the agent is doing first-run setup.
- A user asks "how do I set devboy up for this repo?" or equivalent.

## Procedure

### 1. Detect plugin context

```bash
if [ -n "$CLAUDE_PLUGIN_DATA" ]; then
  PLUGIN_CONTEXT=claude
  PLUGIN_DATA="$CLAUDE_PLUGIN_DATA"
  AGENT=claude
elif [ -n "$CODEX_PLUGIN_DATA" ]; then
  PLUGIN_CONTEXT=codex
  PLUGIN_DATA="$CODEX_PLUGIN_DATA"
  AGENT=codex
else
  PLUGIN_CONTEXT=manual
  AGENT=auto    # let `devboy onboard` autodetect
fi
```

### 2. Ensure `devboy` is on PATH

```bash
if command -v devboy >/dev/null 2>&1; then
  echo "devboy already installed: $(devboy --version)"
else
  # Plugin context: prefer npm, fall back to GitHub Release binary in ${PLUGIN_DATA}/bin
  if command -v npm >/dev/null 2>&1; then
    npm install -g @devboy-tools/cli
  elif [ -n "$PLUGIN_DATA" ]; then
    # GitHub Release binary fallback. Releases are not signed today; we
    # *do* publish a per-asset SHA-256 file alongside each tarball, and
    # the steps below verify it before extracting.
    case "$(uname -s)" in
      Linux*)  os=linux ;;
      Darwin*) os=macos ;;
      *) echo "Unsupported OS: $(uname -s)" >&2; exit 1 ;;
    esac
    case "$(uname -m)" in
      x86_64|amd64)  arch=x86_64 ;;
      aarch64|arm64) arch=arm64 ;;
      *) echo "Unsupported arch: $(uname -m)" >&2; exit 1 ;;
    esac
    asset="devboy-${os}-${arch}.tar.gz"
    base="https://github.com/meteora-pro/devboy-tools/releases/latest/download"
    mkdir -p "$PLUGIN_DATA/bin"
    cd "$PLUGIN_DATA"
    curl -sSL "$base/$asset"        -o "$asset"
    curl -sSL "$base/$asset.sha256" -o "$asset.sha256"
    shasum -a 256 -c "$asset.sha256"            # aborts on mismatch
    tar -xzf "$asset" -C "$PLUGIN_DATA/bin/"
    chmod +x "$PLUGIN_DATA/bin/devboy"
    export PATH="$PLUGIN_DATA/bin:$PATH"
    cd - >/dev/null
  else
    echo "ERROR: neither npm nor a plugin data dir is available — cannot install devboy automatically."
    echo "Install manually with: npm install -g @devboy-tools/cli"
    exit 1
  fi
fi
```

If the binary was installed under `$PLUGIN_DATA/bin`, **tell the user to run `/reload-plugins`** in Claude Code so the MCP entry picks up the new binary on PATH. The MCP server will not connect until reload.

### 3. Run `devboy onboard`

`devboy onboard` (ADR-017) detects the active AI agent (or uses `--agent`), picks a profile, and installs the curated skill bundle into the right directory.

```bash
devboy onboard --agent "$AGENT" --yes
```

In plugin context, `--agent claude` (or `--agent codex`) is forced because we already know which agent loaded us. `onboard` reads `~/.claude/settings.json#enabledPlugins` and **skips** installing skills into `~/.claude/skills/` when this plugin is already there (ADR-018 §5) — it only configures providers.

For manual context (`--agent auto`), `onboard` falls back to its `freshness × volume` scorer and asks the user to confirm.

### 4. Configure providers (if not already done by onboard)

`onboard` covers detection and skill install but does not collect credentials. Run for each provider the user wants to exercise:

```bash
# GitHub
devboy config set github.owner <owner>
devboy config set github.repo <repo>
devboy config set-secret github.token          # prompts for the value

# GitLab
devboy config set gitlab.url https://gitlab.com
devboy config set gitlab.project_id <owner/repo or numeric id>
devboy config set-secret gitlab.token

# ClickUp
devboy config set clickup.list_id <list id>
devboy config set clickup.team_id <team id>
devboy config set-secret clickup.token

# Jira (Cloud)
devboy config set jira.url https://<company>.atlassian.net
devboy config set jira.project_key <KEY>
devboy config set jira.email <email>
devboy config set-secret jira.token
```

On CI / headless hosts where the OS keychain is unavailable, set the corresponding env vars instead (`DEVBOY_GITHUB_TOKEN`, `DEVBOY_GITLAB_TOKEN`, …). See the README for the full fallback chain.

If the user passed a `--remote-config-url`, prefer that path — it keeps the machine's local config minimal and avoids drift:

```bash
devboy init --yes \
  --remote-config-url "<URL from the user>" \
  --remote-config-token "<token>"
```

By design, `--remote-config-url` suppresses local git auto-detection.

### 5. Verify

```bash
devboy test github            # once per configured provider
devboy doctor                 # overall health check
```

Both must print green. If either flags a failure, stop and switch to `repair`.

### 6. Bootstrap secrets (when the project ships a manifest)

Skip this step entirely when the project has no `.devboy/secrets.toml` — it's a noop on legacy projects that pre-date the secret framework. Backward-compat: a project without the manifest stays exactly as it was before this skill version landed.

Detection — walk from CWD up to the filesystem root looking for the manifest:

```bash
manifest=""
dir=$(pwd)
while [ "$dir" != "/" ]; do
  if [ -f "$dir/.devboy/secrets.toml" ]; then
    manifest="$dir/.devboy/secrets.toml"
    break
  fi
  dir=$(dirname "$dir")
done
```

If `manifest` is empty, log `"no .devboy/secrets.toml found in this project tree — skipping secrets bootstrap"` and continue to step 7.

When the manifest exists, count the `required` paths it declares:

```bash
required_count=$(devboy secrets list --json 2>/dev/null \
  | jq '[.[] | select(.required == true)] | length' 2>/dev/null \
  || echo 0)
```

`devboy secrets list --json` already merges the global index with the project manifest (ADR-020 §6), so the count reflects the user's actual setup, not the raw TOML. The `|| echo 0` makes the step safe on systems without `jq` — the worst case is the bootstrap is skipped.

Three branches:

- **`required_count == 0`** — manifest exists but only declares optional paths. Log `"manifest present but no required paths — skipping setup-secrets"` and continue.
- **`required_count > 0` and the user is in an interactive session** — invoke the dedicated wizard:

  ```text
  Skill: setup-secrets
  ```

  The wizard's eight-step flow handles the rest (P16.1). Resume semantics are built in — if the user already ran `setup-secrets` once, the wizard picks up at the first non-done step.
- **`required_count > 0` in a non-interactive context** (CI, scripted run, the user passed `--yes` to the parent setup) — emit a clear instruction line:

  > "This project requires `<N>` secrets via `.devboy/secrets.toml`. Run `setup-secrets` (or `devboy secrets ui`) to provision them before the next CI step that depends on a value."

  …then continue. The setup skill is not the right place to walk through value entry without a human at the keyboard.

### 7. Confirm the tool bundle is wired

```bash
devboy tools list
devboy tools call get_issues '{"limit": 3}'
```

If `tools list` is empty or the tool call returns `ProviderUnsupported`, something is misconfigured — go to `repair`.

## Success criteria

- `command -v devboy` resolves; `devboy --version` reports the expected version.
- `devboy onboard --yes` exits 0 with an `installed` line for the right agent (or a `skipped — already provided by plugin` line in plugin context).
- `devboy doctor` reports every configured provider as healthy.
- `devboy test <provider>` succeeds for each provider the user cares about.
- At least one real tool call (`get_issues`, `get_merge_requests`, or equivalent) returns data rather than `ProviderUnsupported`.
- In plugin context, `claude mcp list` shows `devboy` as registered after `/reload-plugins`.
- When `.devboy/secrets.toml` is present and declares required paths, either `setup-secrets` was invoked (interactive) or the user was instructed to run it next (non-interactive). On legacy projects without a manifest, step 6 is silently skipped.

## Non-goals

- This skill does not configure a proxy MCP server. Use `devboy proxy add` with its documented flags.
- This skill does not migrate an existing setup between machines — it assumes a fresh install.
- This skill does not pick which agent to install skills for in manual mode — `devboy onboard` does that.
- This skill does not provision secret values — even when step 6 detects a manifest, the actual provisioning is delegated to `setup-secrets` so the eight-step idempotent flow stays in one place.
