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
| `--proxy` | | Add MCP proxy server URL |
| `--proxy-only` | | Only configure proxy, skip git remote auto-detection (requires `--proxy`) |
| `--proxy-name` | | Proxy server name (default: "proxy", requires `--proxy`) |
| `--proxy-transport` | | Transport type: "streamable-http" or "sse" (default: "streamable-http", requires `--proxy`) |
| `--proxy-token` | | Proxy token value (stored in keychain automatically, requires `--proxy`) |
| `--proxy-token-key` | | Custom keychain key for token (default: "proxy.{name}.token", requires `--proxy`) |
| `--proxy-auth-type` | | Auth type: "bearer", "api_key", or "none" (default: "bearer" if token, else "none") |
| `--remote-config-url` | | Remote config endpoint; sets `[remote_config]` and, by default, **suppresses git auto-detection** (remote config is treated as the source of truth for integrations) |
| `--remote-config-token` | | Bearer token for the remote config endpoint (stored in keychain as `remote_config.token`, requires `--remote-config-url`) |
| `--detect-git` | | Force git remote auto-detection even when `--remote-config-url` is set (restores the pre-#151 behaviour for users who want both a remote config and an auto-detected local provider) |

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

### Initialize with MCP proxy server

```bash
devboy init --yes \
  --proxy "https://mcp.example.com/api" \
  --proxy-name my-server \
  --proxy-token "your-token-here"
```

This creates configuration with both the auto-detected Git provider and a proxy connection. The token is automatically stored in keychain as `proxy.my-server.token`.

You can also use a custom token key:

```bash
devboy init --yes \
  --proxy "https://mcp.example.com/api" \
  --proxy-name my-server \
  --proxy-token "your-token-here" \
  --proxy-token-key "my.custom.key"
```

### Initialize with proxy only (no Git provider)

If you only want to configure a proxy server without auto-detecting Git remotes:

```bash
devboy init --yes \
  --proxy "https://mcp.example.com/api" \
  --proxy-only \
  --proxy-name my-server \
  --proxy-token "your-token-here" \
  --claude
```

This skips Git remote detection and creates a minimal config with only the proxy server configured.

See [MCP proxy](../configuration/proxy) for more details on proxy configuration.

### Initialize with a remote config endpoint

```bash
devboy init --yes \
  --remote-config-url "https://app.devboy.pro/api/config/mcp" \
  --remote-config-token "your-token-here" \
  --claude
```

Generates a minimal `.devboy.toml` that points at the remote config endpoint:

```toml
[remote_config]
url = "https://app.devboy.pro/api/config/mcp"
token_key = "remote_config.token"
```

**By default, `--remote-config-url` suppresses git remote auto-detection.** The remote config endpoint is treated as the source of truth for integrations, so devboy does not add a local `[contexts.*.github]` / `[contexts.*.gitlab]` section from the current `origin` — a local section would almost always be stale and drift from whatever the remote is actually serving.

If you genuinely want both — a remote config *and* an auto-detected local provider pinned at bootstrap time — add `--detect-git`:

```bash
devboy init --yes \
  --remote-config-url "https://app.devboy.pro/api/config/mcp" \
  --remote-config-token "your-token-here" \
  --detect-git
```

## Auto-detection

When using `--yes` or in interactive mode, devboy automatically detects:

- **GitHub** repositories from SSH (`git@github.com:owner/repo.git`) or HTTPS (`https://github.com/owner/repo`) remotes
- **GitLab** repositories from SSH (`git@gitlab.com:owner/repo.git`) or HTTPS (`https://gitlab.com/owner/repo`) remotes

The detected provider is pre-selected in interactive mode or automatically configured in `--yes` mode.

**Auto-detection is suppressed when:**

- `--proxy-only` is passed (requires `--proxy`)
- `--remote-config-url` is passed without `--detect-git`

In both cases the rationale is the same — an external source (a proxy server or a remote config endpoint) is expected to supply the integration settings, so a locally auto-detected provider would just create drift.

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
