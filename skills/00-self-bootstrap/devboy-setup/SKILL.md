---
name: devboy-setup
description: Walk a user through configuring devboy-tools from scratch — providers, credentials, Claude Code registration, verification.
category: self-bootstrap
version: 1
compatibility: devboy-tools >= 0.18
activation:
  - "setup devboy"
  - "configure devboy"
  - "initialise devboy"
  - "install devboy"
tools:
  - init
  - config
  - doctor
  - test
---

# devboy-setup

Configure `devboy-tools` end-to-end for the current project or the current user. The skill handles the happy path and the common "something is not quite right" branches; for a broken state that needs triage, use `devboy-repair`.

## When to use

- A new clone of a project and `.devboy.toml` is missing.
- A new machine and `devboy doctor` reports no configured providers.
- A user asks "how do I set devboy up for this repo?" or equivalent.

## Preconditions

1. `devboy` is on `PATH`. If it is not:

   ```bash
   devboy tools call doctor '{}' >/dev/null 2>&1 || \
     echo "devboy is not on PATH — install it via 'npm install -g @devboy-tools/cli' first"
   ```

2. The user knows which provider(s) they want to configure (GitHub, GitLab, ClickUp, Jira, Slack, Fireflies).

## Procedure

### 1. Pick the install target

- If a `.devboy.toml` already exists in the repo root, the skill is operating on an already-initialised project — jump to **step 4**.
- If not, run `devboy init` interactively or use `--yes` for auto-detection:

  ```bash
  devboy init --yes
  ```

  This auto-detects GitHub / GitLab from the `origin` remote and creates `.devboy.toml`.

### 2. Non-interactive bootstrap with remote config

When the user mentions a remote configuration endpoint, prefer that path — it keeps the machine's local config minimal and avoids drift:

```bash
devboy init --yes \
  --remote-config-url "<URL from the user>" \
  --remote-config-token "<token>" \
  --claude
```

By design (ADR-DEV-798), `--remote-config-url` suppresses the local git auto-detection — the remote endpoint is treated as the source of truth for integrations.

### 3. Register with Claude Code

If the `--claude` flag was not passed during init, register separately:

```bash
devboy init --claude
# or, equivalently:
claude mcp add devboy -- devboy mcp
```

### 4. Add per-provider tokens

For each provider the user wants to exercise:

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

On CI / headless hosts where the OS keychain is unavailable, set the corresponding env vars instead (`DEVBOY_GITHUB_TOKEN`, `DEVBOY_GITLAB_TOKEN`, …). See the `README` for the full fallback chain.

### 5. Verify

```bash
devboy test github            # once per configured provider
devboy doctor                 # overall health check
```

Both must print green. If either flags a failure, stop and fall through to `devboy-repair`.

### 6. Confirm the tool bundle is wired

```bash
devboy tools list
devboy tools call get_issues '{"limit": 3}'
```

If `tools list` is empty or the tool call returns `ProviderUnsupported`, something was misconfigured — go to `devboy-repair`.

## Success criteria

- `devboy doctor` reports every configured provider as healthy.
- `devboy test <provider>` succeeds for each provider the user cares about.
- At least one real tool call (`get_issues`, `get_merge_requests`, or the equivalent for the provider) returns data rather than `ProviderUnsupported`.
- If the user asked for Claude Code integration, `claude mcp list` shows `devboy`.

## Non-goals

- This skill does not configure a proxy MCP server. Use `devboy proxy add` with its documented flags.
- This skill does not migrate an existing setup between machines — it assumes a fresh install.
