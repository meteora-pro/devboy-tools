# Quick Start

This guide will help you get DevBoy Tools up and running with GitHub integration in minutes.

## Step 1: Create a GitHub Token

Before configuring DevBoy, you need a GitHub personal access token.

For detailed instructions, see the [GitHub Integration](/integrations/github#required-token-scopes) page.

**Quick summary:**
1. Go to GitHub → Settings → Developer settings → Personal access tokens → Tokens (classic)
2. Click **Generate new token (classic)**
3. Give it a descriptive name (e.g., "DevBoy Tools")
4. Select the `repo` and `read:user` scopes
5. Click **Generate token** and copy it

## Step 2: Configure DevBoy

Set up your GitHub integration:

```bash
# Set the repository owner (user or organization)
devboy config set github.owner <your-github-username>

# Set the repository name
devboy config set github.repo <your-repo-name>

# Store the token securely in OS keychain
devboy config set-secret github.token <your-token>
```

## Step 3: Verify Connection

Test that everything is working:

```bash
devboy test github
```

You should see output confirming the connection is successful.

## Step 4: Try Some Commands

### List Issues

```bash
devboy issues
```

### List Pull Requests

```bash
devboy mrs
```

## Step 5: Integrate with AI Assistants

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
