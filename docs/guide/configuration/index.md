# Configuration

DevBoy tools uses a layered configuration system with TOML files and OS keychain for secrets.

## Configuration files

### Global config

The main configuration file is stored in a platform-specific location:

| Platform | Path |
|----------|------|
| **macOS/Linux** | `~/.config/devboy-tools/config.toml` |
| **Windows** | `%APPDATA%\devboy-tools\config.toml` |

```bash
# Show the config file path
devboy config path
```

### Project-local config

You can place a `.devboy.toml` file in your project root to override the global config. This is useful when working with AI assistants (MCP mode) in a specific repository.

```toml
# .devboy.toml (project root)
[contexts.my-project.github]
owner = "my-org"
repo = "my-project"
```

**Resolution order:**
1. `.devboy.toml` in the current directory (highest priority)
2. `~/.config/devboy-tools/config.toml` (global fallback)

:::tip
Add `.devboy.toml` to your `.gitignore` — it contains project-specific settings that may differ between contributors.
:::

## Secrets (Tokens)

Tokens are stored in the OS keychain (macOS Keychain, Windows Credential Manager, Linux Secret Service) and are never written to config files.

```bash
devboy config set-secret github.token <token>
devboy config set-secret gitlab.token <token>
```

## MCP Proxy

You can proxy tools from upstream MCP servers through DevBoy. See [MCP Proxy](./proxy) for details.

```toml
[[proxy_mcp_servers]]
name = "devboy-cloud"
url = "https://app.devboy.pro/api/mcp?name=my-project"
auth_type = "bearer"
token_key = "devboy-cloud.token"
transport = "streamable-http"
```

## CLI commands

```bash
# Set a config value
devboy config set <key> <value>

# Get a config value
devboy config get <key>

# Store a secret in keychain
devboy config set-secret <key> <value>

# List all configuration
devboy config list

# Show config file path
devboy config path

# List proxied tools from upstream servers
devboy proxy tools

# Call a proxied tool
devboy proxy call <tool_name> [args_json]
```
