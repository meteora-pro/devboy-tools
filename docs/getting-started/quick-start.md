# Quick Start

This guide will help you get DevBoy Tools up and running in minutes.

## Step 1: Choose Your Provider

DevBoy Tools supports GitHub, GitLab, ClickUp, and Jira. Pick the one your project uses.

### GitHub

1. Go to GitHub → Settings → Developer settings → Personal access tokens → Tokens (classic)
2. Click **Generate new token (classic)**
3. Select the `repo` and `read:user` scopes
4. Click **Generate token** and copy it

```bash
devboy config set github.owner <owner>
devboy config set github.repo <repo>
devboy config set-secret github.token <token>
```

### GitLab

1. Go to GitLab → User Settings → Access Tokens
2. Click **Add new token**
3. Select the `api` and `read_user` scopes
4. Click **Create personal access token** and copy it

```bash
devboy config set gitlab.url <instance-url>
devboy config set gitlab.project_id <project-id>
devboy config set-secret gitlab.token <token>
```

### Jira

1. For Jira Cloud: Go to https://id.atlassian.com/manage-profile/security/api-tokens
2. Click **Create API token**, give it a label, and copy it

```bash
devboy config set jira.url https://company.atlassian.net
devboy config set jira.project_key PROJ
devboy config set jira.email user@example.com
devboy config set-secret jira.token <token>
```

> **Tip:** Use the Quick Config Generator on the [GitHub](/integrations/github), [GitLab](/integrations/gitlab), or [Jira](/integrations/jira) integration page — paste your URL and it will generate the commands for you.

## Step 2: Verify Connection

```bash
# For GitHub
devboy test github

# For GitLab
devboy test gitlab

# For Jira
devboy test jira
```

You should see output confirming the connection is successful.

## Step 3: Try Some Commands

### List Issues

```bash
devboy issues
```

### List Merge Requests / Pull Requests

```bash
devboy mrs
```

## Step 4: Integrate with AI Assistants

### Claude Code (CLI)

```bash
claude mcp add devboy -- /path/to/devboy mcp
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

## Next Steps

- [GitHub Integration](/integrations/github) - Full GitHub configuration reference
- [GitLab Integration](/integrations/gitlab) - Full GitLab configuration reference
- [Jira Integration](/integrations/jira) - Full Jira configuration reference
