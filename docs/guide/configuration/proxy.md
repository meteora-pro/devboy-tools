# MCP proxy

DevBoy can proxy tool calls to upstream MCP servers, exposing their tools alongside its own. This lets you combine tools from multiple MCP servers into a single endpoint.

## Quick setup

The fastest way to add a proxy server:

```bash
# Add proxy server with token (stored in keychain automatically)
devboy proxy add my-server \
  --url "https://mcp.example.com/api" \
  --token "your-token-here"

# Verify available tools
devboy proxy tools
```

Or during project initialization:

```bash
devboy init --yes \
  --proxy "https://mcp.example.com/api" \
  --proxy-name my-server \
  --proxy-token "your-token-here"
```

The token is automatically stored in keychain as `proxy.my-server.token`.

## Use case

You have a remote MCP server with additional tools (knowledge base, meeting notes, messengers). Instead of configuring multiple MCP servers in your AI assistant, you configure DevBoy to proxy them all through one connection.

## Configuration

Add upstream servers to your `config.toml` or `.devboy.toml`:

```toml
[[proxy_mcp_servers]]
name = "devboy-cloud"
url = "https://mcp.example.com/api"
auth_type = "bearer"
token_key = "devboy-cloud.token"
transport = "streamable-http"
```

Store the token in keychain:

```bash
devboy config set-secret devboy-cloud.token <YOUR_TOKEN>
```

### Fields

| Field | Required | Default | Description |
|-------|----------|---------|-------------|
| `name` | yes | — | Server name, used as tool prefix if `tool_prefix` not set |
| `url` | yes | — | Server URL (SSE or Streamable HTTP endpoint) |
| `auth_type` | no | `"none"` | Authentication type: `"bearer"`, `"api_key"`, or `"none"` |
| `token_key` | no | — | Keychain key for the auth token |
| `tool_prefix` | no | `name` | Custom prefix for proxied tool names |
| `transport` | no | `"sse"` | Transport protocol: `"sse"` or `"streamable-http"` |

### Transport types

- **`sse`** — Legacy MCP transport. Uses GET for SSE stream, POST for requests. Used by most self-hosted MCP servers.
- **`streamable-http`** — Modern HTTP POST-based transport with `mcp-session-id` header. Used by hosted MCP services.

### Multiple servers

You can proxy multiple upstream servers:

```toml
[[proxy_mcp_servers]]
name = "devboy-cloud"
url = "https://mcp.example.com/api?name=project-a"
auth_type = "bearer"
token_key = "devboy-cloud.token"
transport = "streamable-http"

[[proxy_mcp_servers]]
name = "internal-tools"
url = "http://localhost:3001/sse"
tool_prefix = "internal"
```

## How it works

1. On startup, DevBoy connects to each configured upstream server and performs the MCP `initialize` handshake.
2. Upstream tools are fetched and exposed with a prefix: `<prefix>__<tool_name>` (e.g. `devboy-cloud__get_issues`).
3. When a proxied tool is called, DevBoy strips the prefix and forwards the request to the matching upstream server.

## CLI commands

### Add a proxy server

Add a new proxy server without editing the config file manually:

```bash
# Basic usage
devboy proxy add my-server --url "https://example.com/mcp"

# With all options
devboy proxy add devboy-cloud \
  --url "https://mcp.example.com/api" \
  --transport streamable-http \
  --token-key devboy-cloud.token

# Overwrite existing proxy
devboy proxy add my-server --url "https://new.example.com/mcp" --force
```

| Option | Default | Description |
|--------|---------|-------------|
| `--url` | (required) | Proxy server URL |
| `--transport` | `streamable-http` | Transport type: `streamable-http` or `sse` |
| `--token` | — | Token value (stored in keychain automatically) |
| `--token-key` | `proxy.{name}.token` | Custom keychain key for token |
| `--auth-type` | `bearer` if token, else `none` | Auth type: `bearer`, `api_key`, or `none` |
| `--force` | `false` | Overwrite existing proxy with same name |

### Remove a proxy server

```bash
devboy proxy remove my-server
```

### List proxied tools

```bash
# Tool names only
devboy proxy tools

# With descriptions
devboy proxy tools --descriptions
```

### Call a proxied tool

```bash
# With arguments
devboy proxy call devboy-cloud__get_issues '{"state": "open"}'

# Without arguments
devboy proxy call devboy-cloud__get_project_info
```

## MCP server integration

When running as an MCP server (`devboy mcp`), proxied tools are automatically included in `tools/list` and routed via `tools/call`. No additional configuration is needed on the client side — AI assistants see all tools (both local and proxied) as a flat list.
