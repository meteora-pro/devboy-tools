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

## Built-in tools

By default all built-in tools are enabled. You can disable specific tools to reduce LLM context size — disabled tools won't appear in `tools/list` and won't consume tokens.

DevBoy uses a **blacklist** approach: all tools are enabled by default, and you explicitly disable the ones you don't need. This means any new tools added in future versions will be available automatically.

### Configuration

```toml
[builtin_tools]
disabled = ["create_issue", "update_issue", "add_issue_comment"]
```

### CLI commands

```bash
# Interactive mode — toggle tools with checkboxes
devboy tools

# List all tools with their status
devboy tools list

# Disable specific tools
devboy tools disable create_issue update_issue

# Re-enable specific tools
devboy tools enable create_issue

# Reset all filtering (re-enable everything)
devboy tools reset

# Call a built-in tool directly
devboy tools call list_contexts
devboy tools call get_issues '{"state":"open","limit":5}'
```

### Available built-in tools

| Tool | Description |
|------|-------------|
| `get_issues` | List issues |
| `get_issue` | Get issue details |
| `get_issue_comments` | Get issue comments |
| `create_issue` | Create a new issue |
| `update_issue` | Update an issue |
| `add_issue_comment` | Add comment to issue |
| `get_merge_requests` | List merge requests |
| `get_merge_request` | Get MR details |
| `get_merge_request_discussions` | Get MR discussions |
| `get_merge_request_diffs` | Get MR diffs |
| `create_merge_request` | Create a new MR |
| `create_merge_request_comment` | Add comment to MR |
| `list_contexts` | List available contexts |
| `use_context` | Switch active context |
| `get_current_context` | Get current context |

:::tip
Proxy tools from upstream MCP servers are **not** affected by builtin_tools filtering.
:::

## MCP proxy

You can proxy tools from upstream MCP servers through DevBoy. See [MCP proxy](./proxy) for details.

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

# Manage built-in tools (interactive mode)
devboy tools

# List tools with status
devboy tools list

# Disable/enable/reset tools
devboy tools disable <names...>
devboy tools enable <names...>
devboy tools reset

# Call a built-in tool directly
devboy tools call <name> [args_json]
```
