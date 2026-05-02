---
name: setup
description: Bootstrap devboy from scratch — install the CLI if missing, register the MCP server, run `devboy onboard` for the active agent, verify with `doctor`. First-run skill for both manual installs and the Claude Code / Codex plugin.
category: self-bootstrap
version: 2
compatibility: devboy-tools >= 0.24
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
---

# devboy-setup

Bring `devboy` from "nothing on this machine" to "MCP server registered, providers configured, doctor green". Works in two modes:

- **Plugin-driven** — when invoked from the Claude Code plugin (`devboy@meteora-devboy`) or the Codex plugin. The plugin manifest is already loaded; this skill installs the CLI binary and runs `devboy onboard` to wire up the rest. Detected by `$CLAUDE_PLUGIN_DATA` (Claude) or `$CODEX_PLUGIN_DATA` (Codex) being set.
- **Manual** — when the user invoked the skill directly after `npm install -g @devboy-tools/cli`. The CLI is already present; only configuration remains.

For a broken state that needs triage, use `devboy-repair` instead.

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
    mkdir -p "$PLUGIN_DATA/bin"
    PLATFORM="$(uname -s | tr '[:upper:]' '[:lower:]')-$(uname -m)"
    curl -sSL "https://github.com/meteora-pro/devboy-tools/releases/latest/download/devboy-${PLATFORM}" \
      -o "$PLUGIN_DATA/bin/devboy"
    chmod +x "$PLUGIN_DATA/bin/devboy"
    export PATH="$PLUGIN_DATA/bin:$PATH"
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

Both must print green. If either flags a failure, stop and switch to `devboy-repair`.

### 6. Confirm the tool bundle is wired

```bash
devboy tools list
devboy tools call get_issues '{"limit": 3}'
```

If `tools list` is empty or the tool call returns `ProviderUnsupported`, something is misconfigured — go to `devboy-repair`.

## Success criteria

- `command -v devboy` resolves; `devboy --version` reports the expected version.
- `devboy onboard --yes` exits 0 with an `installed` line for the right agent (or a `skipped — already provided by plugin` line in plugin context).
- `devboy doctor` reports every configured provider as healthy.
- `devboy test <provider>` succeeds for each provider the user cares about.
- At least one real tool call (`get_issues`, `get_merge_requests`, or equivalent) returns data rather than `ProviderUnsupported`.
- In plugin context, `claude mcp list` shows `devboy` as registered after `/reload-plugins`.

## Non-goals

- This skill does not configure a proxy MCP server. Use `devboy proxy add` with its documented flags.
- This skill does not migrate an existing setup between machines — it assumes a fresh install.
- This skill does not pick which agent to install skills for in manual mode — `devboy onboard` does that.
