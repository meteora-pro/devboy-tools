# Project initialization

The `devboy init` command provides an interactive wizard to create a `.devboy.toml` configuration file and securely store API tokens in your OS keychain.

## Basic usage

### Interactive mode (recommended)

Run without arguments for a guided setup:

```bash
devboy init
```

This will:
1. Prompt you for a context name (defaults to current directory name)
2. Let you select which providers to configure (GitHub, GitLab, ClickUp, Jira)
3. Guide you through each provider's configuration
4. Optionally store API tokens securely in your OS keychain

### Non-interactive mode

For CI/CD or scripted setups, use the `--yes` flag:

```bash
devboy init --yes
```

This automatically detects your Git provider from the `origin` remote and creates a minimal configuration.

## Command options

| Flag | Short | Description |
|------|-------|-------------|
| `--yes` | `-y` | Skip prompts, use defaults (auto-detect from git remote) |
| `--dry-run` | | Preview changes without writing files |
| `--force` | `-f` | Overwrite existing config (creates timestamped backup) |
| `--claude` | | Register devboy as MCP server in Claude Code |
| `--context` | `-c` | Specify context name (default: directory name) |

## Examples

### Preview what would be created

```bash
devboy init --dry-run
```

Output:
```
Detected GitHub repository: owner/repo
[dry-run] Would create: .devboy.toml

[contexts.my-project.github]
owner = "owner"
repo = "repo"
```

### Initialize with Claude Code integration

```bash
devboy init --claude
```

This creates the configuration and registers devboy as an MCP server in Claude Code, enabling AI-assisted development workflows.

### Force reinitialize with backup

```bash
devboy init --force
```

If `.devboy.toml` already exists, this creates a timestamped backup (e.g., `.devboy.toml.backup.20250316_143022`) before overwriting.

### Specify custom context name

```bash
devboy init --context production
```

Creates configuration with context named "production" instead of the directory name.

## Auto-detection

When using `--yes` or in interactive mode, devboy automatically detects:

- **GitHub** repositories from SSH (`git@github.com:owner/repo.git`) or HTTPS (`https://github.com/owner/repo`) remotes
- **GitLab** repositories from SSH (`git@gitlab.com:owner/repo.git`) or HTTPS (`https://gitlab.com/owner/repo`) remotes

The detected provider is pre-selected in interactive mode or automatically configured in `--yes` mode.

## Token storage

Tokens are stored securely using your operating system's native credential manager:

- **macOS**: Keychain Services
- **Windows**: Credential Manager
- **Linux**: Secret Service (GNOME Keyring / KWallet)

In interactive mode, you'll be prompted to store tokens. If a token already exists, you'll be asked whether to overwrite it.

## Generated configuration

The init command creates a `.devboy.toml` file in the current directory:

```toml
[contexts.my-project.github]
owner = "owner"
repo = "repo"

[contexts.my-project.gitlab]
url = "https://gitlab.com"
project_id = "owner/repo"

[contexts.my-project.clickup]
list_id = "12345678"
team_id = "1234567"

[contexts.my-project.jira]
url = "https://company.atlassian.net"
project_key = "PROJ"
email = "user@example.com"
```

## Next steps

After initialization:

1. **Verify your configuration**: `cat .devboy.toml`
2. **Test provider connection**: `devboy test github` (or gitlab, clickup, jira)
3. **Start using devboy**: `devboy issues` or `devboy mcp`

See the [Configuration guide](../configuration/) for advanced configuration options.
