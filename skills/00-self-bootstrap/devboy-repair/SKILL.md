---
name: devboy-repair
description: Diagnose and fix a broken devboy-tools setup — corrupt config, missing tokens, keychain trouble, wrong paths, plugin install failures.
category: self-bootstrap
version: 2
compatibility: devboy-tools >= 0.24
activation:
  - "repair devboy"
  - "fix devboy"
  - "devboy is broken"
  - "devboy doctor failing"
  - "devboy plugin not working"
tools:
  - doctor
  - config
  - test
---

# devboy-repair

Walk a misbehaving `devboy-tools` setup back to health. This skill is driven by `devboy doctor --format json` — the structured output is the source of truth for what's wrong, and every repair step maps to a diagnostic code.

## When to use

- `devboy doctor` exits non-zero.
- Tool calls return `ProviderUnsupported` unexpectedly.
- The user reports "it worked yesterday, now it does not".
- `devboy test <provider>` prints a 401 / 403 / network error.

If the issue is "nothing is configured yet" — use `devboy-setup` instead. This skill assumes a prior configuration existed.

## Procedure

### 1. Plugin context first

Before running `doctor`, check whether we are in plugin context (Claude Code plugin or Codex plugin) and whether the binary is reachable at all:

```bash
if [ -n "$CLAUDE_PLUGIN_DATA" ]; then PLUGIN_CONTEXT=claude; PLUGIN_DATA="$CLAUDE_PLUGIN_DATA"; fi
if [ -n "$CODEX_PLUGIN_DATA"  ]; then PLUGIN_CONTEXT=codex;  PLUGIN_DATA="$CODEX_PLUGIN_DATA";  fi

command -v devboy || ls -la "${PLUGIN_DATA:-/dev/null}/bin/devboy" 2>/dev/null
```

If the binary is **missing entirely**, the plugin's `setup` skill failed during install. Recover before continuing:

- **npm path failed** (sudo refused, restrictive prefix, npm not installed) — fall back to the signed GitHub Release binary:

  ```bash
  PLATFORM="$(uname -s | tr '[:upper:]' '[:lower:]')-$(uname -m)"
  mkdir -p "$PLUGIN_DATA/bin"
  curl -sSL "https://github.com/meteora-pro/devboy-tools/releases/latest/download/devboy-${PLATFORM}" \
    -o "$PLUGIN_DATA/bin/devboy"
  chmod +x "$PLUGIN_DATA/bin/devboy"
  export PATH="$PLUGIN_DATA/bin:$PATH"
  ```

- **Binary present but MCP not connected** — Claude Code or Codex needs `/reload-plugins` (or a session restart) for the MCP server entry to pick up a newly installed binary. Tell the user to run `/reload-plugins` and try again.

- **Plugin enabled but skills missing in the agent's skill catalogue** — the agent has not refreshed; either reload or check that `~/.claude/settings.json#enabledPlugins` actually contains `devboy@meteora-devboy`.

Once the binary is reachable, continue with `doctor`.

### 2. Pin the fault

```bash
devboy doctor --format json > /tmp/devboy-doctor.json
jq '.' /tmp/devboy-doctor.json
```

The JSON shape is:

```json
{
  "version": { "current_version": "...", "latest_version": "...", "update_available": false, "install_method": "...", "update_command": "devboy upgrade" },
  "results": [
    { "id": "environment.os_support", "category": "Environment", "name": "...", "status": "pass|warning|error", "message": "...", "details": null, "fix_command": "devboy init", "fix_url": null }
  ]
}
```

Every result has `{ id, category, name, status, message, details, fix_command, fix_url }`. Status values are `pass` (good), `warning` (recoverable but worth attention), and `error` (must be fixed). Any non-null `fix_command` is a suggested starting point.

If the command itself fails to run, `devboy` is not on `PATH` — install or re-link the binary before continuing.

### 3. Classify by check id

The real check id taxonomy (from `devboy doctor --list-checks`):

- **Environment** — `environment.os_support`, `environment.config_dir`, `environment.credential_store`. The first two warn when the config directory is missing (run `devboy init`); the third warns when the OS keychain daemon isn't reachable (move tokens to env vars — see step 3).
- **Configuration** — `config.exists`, `config.valid_toml`, `config.active_context`. Missing file → `devboy init`; invalid TOML → open `.devboy.toml` in an editor or run `devboy init --force`; stale active context → edit the `active_context` field or re-run `devboy init`.
- **Credentials** — `credentials.github`, `credentials.gitlab`, `credentials.clickup`, `credentials.jira`, `credentials.slack`. A `warning`/`error` means the token is missing. Store it with `devboy config set-secret <provider>.token` or set the matching `DEVBOY_<PROVIDER>_TOKEN` / `<PROVIDER>_TOKEN` env var.
- **Provider Connectivity** — `providers.github`, `providers.gitlab`, `providers.clickup`, `providers.jira`, `providers.slack`. `error` means the token is rejected (401/403) or the endpoint is unreachable. 401/403 → rotate the token. Unreachable → check network / base URL.
- **MCP Server** — `mcp.tools` reports on the built-in tool filter; only warns if the config disables every tool.
- **Proxy** — `proxy.servers` checks upstream MCP proxy connectivity. Failure usually means a bad `--proxy-token` or a dead URL; re-issue with `devboy proxy add <name> --url <url> --force --token <new>`.

### 4. Re-verify

After each fix:

```bash
devboy doctor --format json | jq '[.results[] | select(.status=="error")] | length'
```

Zero `error` results is the target (some `warning`s are expected — e.g. "no config file" until `devboy init` runs). Repeat step 3 until every `error` is resolved.

### 5. Smoke-test the tool bundle

```bash
devboy tools list
devboy tools call get_issues '{"limit": 3}'
```

Either must produce real data. `ProviderUnsupported` at this point means the provider is mis-configured (wrong project id, wrong list id, wrong repo owner) — go back to step 2.

## Guardrails

- **Never print token values** into the chat. When the user asks "what is my token?" the answer is "it lives in your keychain — re-issue it from the provider if you need a copy". Treat every `*_token` / `set-secret` argument as opaque.
- **Do not commit changes to `.devboy.toml`** automatically — config changes are a user decision.
- **If two checks disagree**, trust `devboy doctor` — it is the only deterministic source of ground truth here.

## Success criteria

- `devboy doctor` exits zero with no failing checks.
- At least one real tool call against each previously-broken provider succeeds.
- If the session started with a specific complaint from the user (e.g. "get_issues returns nothing"), the exact reported behaviour is now correct.
