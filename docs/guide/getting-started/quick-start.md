# Quick start

Sixty seconds from `npm install` to a working setup with skills wired into your AI agent.

## Step 1: Onboard

After installing the CLI (see [Installation](./)), run:

```bash
devboy onboard
```

This auto-detects which AI agent you actively use (Claude Code, Copilot CLI, Codex, Cursor, Kimi, Gemini, …) by scanning the agent's home directory, picks a primary candidate by recency × volume, and installs a curated skill bundle for that agent.

Profiles let you tailor the bundle:

```bash
devboy onboard                          # default `dev` bundle
devboy onboard --profile pm             # PM bundle (issues, meetings, messengers)
devboy onboard --profile oncall         # diagnostics + notifications
devboy onboard --agent kimi --yes       # explicit agent + non-interactive
devboy agents list                      # show all detected agents with score
```

## Step 2: Initialise your project (interactive)

```bash
devboy init
```

Walks you through picking providers and pasting tokens — they go straight to the OS keychain. For details see [Project initialization](./init).

### Alternative: manual configuration

If you'd rather configure providers by hand, follow the steps below.

#### Choose your provider

DevBoy tools supports GitHub, GitLab, ClickUp, and Jira. Pick the one your project uses.

##### GitHub

1. Go to GitHub → Settings → Developer settings → Personal access tokens → Tokens (classic)
2. Click **Generate new token (classic)**
3. Select the `repo` and `read:user` scopes
4. Click **Generate token** and copy it

```bash
devboy config set github.owner <owner>
devboy config set github.repo <repo>
devboy config set-secret github.token <token>
```

##### GitLab

1. Go to GitLab → User Settings → Access Tokens
2. Click **Add new token**
3. Select the `api` and `read_user` scopes
4. Click **Create personal access token** and copy it

```bash
devboy config set gitlab.url <instance-url>
devboy config set gitlab.project_id <project-id>
devboy config set-secret gitlab.token <token>
```

##### Jira

1. For Jira Cloud: Go to https://id.atlassian.com/manage-profile/security/api-tokens
2. Click **Create API token**, give it a label, and copy it

```bash
devboy config set jira.url https://company.atlassian.net
devboy config set jira.project_key PROJ
devboy config set jira.email user@example.com
devboy config set-secret jira.token <token>
```

> **Tip:** Use the Quick Config Generator on the [GitHub](/integrations/github), [GitLab](/integrations/gitlab), or [Jira](/integrations/jira) integration page — paste your URL and it will generate the commands for you.

## Step 3: Verify connection

```bash
# For GitHub
devboy test github

# For GitLab
devboy test gitlab

# For Jira
devboy test jira
```

You should see output confirming the connection is successful.

## Step 4: Try some commands

### List issues

```bash
devboy issues
```

### List merge requests / pull requests

```bash
devboy mrs
```

## Step 5: Integrate with AI assistants

### Claude Code (CLI)

The easiest way is to use the init command with `--claude` flag:

```bash
devboy init --claude
```

Or register manually:

```bash
claude mcp add devboy -- devboy mcp
```

Verify the integration:
```bash
claude mcp list
```

### Claude Desktop

Add to `~/Library/Application Support/Claude/claude_desktop_config.json` (macOS):

```json
{
  "mcpServers": {
    "devboy": {
      "command": "/path/to/devboy",
      "args": ["mcp"]
    }
  }
}
```

**Windows:** `%APPDATA%\Claude\claude_desktop_config.json`

**Linux:** `~/.config/Claude/claude_desktop_config.json`

## Next steps

- [GitHub Integration](/integrations/github) - Full GitHub configuration reference
- [GitLab Integration](/integrations/gitlab) - Full GitLab configuration reference
- [Jira Integration](/integrations/jira) - Full Jira configuration reference
