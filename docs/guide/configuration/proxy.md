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

## Transparent routing: local fallback for upstream tools

When the same tool is advertised by both the local `ToolHandler` and a connected upstream MCP server, DevBoy can optionally dispatch the call locally instead of round-tripping through the upstream. This is useful when:

- The upstream cannot reach a provider that is available from the developer's network (GitLab / Jira behind corporate VPN).
- The cloud integration is degraded and you want a local fallback.
- You prefer lower latency for interactive tools.

The feature is **opt-in**. By default, every matched call goes to the upstream (cloud has priority).

### Enabling

Add a `[proxy.routing]` section to your `config.toml`:

```toml
[proxy.routing]
# Default strategy for every matched tool.
# One of: "remote", "local", "local-first", "remote-first".
strategy = "local-first"

# If the primary executor errors, retry on the other executor.
# Only meaningful for "local-first" / "remote-first".
fallback_on_error = true

# First-match-wins per-tool overrides (globs with `*`).
[[proxy.routing.tool_overrides]]
pattern = "get_*"
strategy = "local"

[[proxy.routing.tool_overrides]]
pattern = "create_*"
strategy = "remote"       # writes always go upstream
```

### Strategies

| Strategy        | Behaviour                                                                 |
|-----------------|----------------------------------------------------------------------------|
| `remote`        | Always route matched calls to the upstream. **Default.**                   |
| `local`         | Always route matched calls to the local executor.                          |
| `local-first`   | Try local first; fall back to upstream on error (if `fallback_on_error`).  |
| `remote-first`  | Try upstream first; fall back to local on error (if `fallback_on_error`).  |

### Graceful degradation

If the upstream schema requires arguments the local schema does not declare, DevBoy routes that specific tool to the upstream automatically — regardless of the strategy. This keeps existing calls working even when the two implementations drift. You can inspect such mismatches with `devboy proxy status`.

### Per-server override

A `routing` block under `[[proxy_mcp_servers]]` overrides the global policy for that upstream only:

```toml
[[proxy_mcp_servers]]
name = "devboy-cloud"
url = "https://mcp.example.com/api"
auth_type = "bearer"
token_key = "devboy-cloud.token"
transport = "streamable-http"

[proxy_mcp_servers.routing]
strategy = "local-first"
```

## Secrets cache

Local-first routing means secrets come from the OS keychain on every call. A short-lived in-memory cache prevents repeated keychain prompts without compromising rotation semantics.

```toml
[proxy.secrets]
# TTL for the cache, in seconds. Default: 300 (5 minutes).
# Set to 0 to disable caching and always read from the keychain.
cache_ttl_secs = 300
```

- Cached values are zeroized on eviction and on process exit.
- Writing via `devboy config set-secret …` invalidates the corresponding cache entry immediately.
- Set `cache_ttl_secs = 0` for high-security setups where every prompt should hit the keychain directly.

## Telemetry

When routing happens locally the cloud backend loses visibility into usage. DevBoy forwards a minimal event to the configured telemetry endpoint so cloud dashboards stay accurate.

```toml
[proxy.telemetry]
enabled = true
endpoint = "https://app.example.com/api/telemetry/tool-invocations"
batch_size = 100            # flush when this many events accumulate
batch_interval_secs = 30    # or at least once per this many seconds
offline_queue_max = 10000   # drop oldest when the offline queue is full
# Optional keychain key for the telemetry auth token.
# Falls back to the first upstream server's token_key when unset.
# token_key = "devboy-cloud.token"
```

The payload is intentionally minimal — it never contains tool arguments or responses. Only:

- `tool` — unprefixed tool name
- `routing_decision` — short label (`strategy_remote`, `override_rule`, `schema_incompatible`, …)
- `routing_detail` — for `override_rule`, the glob pattern that matched
- `upstream` — prefix when the call went remote
- `status` — `success` / `error`
- `latency_ms` — observed latency
- `timestamp_secs` — unix epoch seconds
- `was_fallback` — true if the primary executor failed and we retried

Set `enabled = false` or omit `endpoint` to collect events locally without uploading (useful for CLI debugging).

## Observability

### `devboy proxy status`

Prints a human-readable snapshot of the routing table: what is routable locally, what stays remote, which pairs have incompatible schemas, and the currently active override rules. Exit with `--json` for a machine-readable form.

### Structured logs

Every routing decision is emitted at `tracing::info` level with fields:

```
tool=get_issues
resolved=get_issues
target=local
reason=strategy_local_first
reason_detail=
has_fallback=true
```

Filter with `RUST_LOG=devboy_mcp::routing=info` to see only routing events.

### Response `_meta.routing`

For the MCP transport, each `tools/call` response carries an optional `_meta.routing` object describing the executor picked. Client tooling can surface this for debug UI.

## Cloud priority — summary of invariants

- The default strategy is `remote`; no local routing happens unless the user opts in.
- Missing upstream schemas disable local routing for that specific tool.
- Telemetry is on by default so cloud usage statistics remain accurate even when calls execute locally.

## Validation rules

### Config CLI (`devboy config set|get`)

Keys under `proxy.{routing|secrets|telemetry}.*` are a **structured schema**. Typos surface as explicit errors, not silent fallbacks — both on write and on read:

```bash
$ devboy config set proxy.routing.strategy teleport
Error: Configuration error: Invalid routing strategy 'teleport'.
       Allowed (case-insensitive): remote, local, local-first, remote-first

$ devboy config get proxy.routing.nonexistent
Error: Configuration error: Unknown proxy.routing field: nonexistent
# exit code: 1
```

Provider paths (`github.*`, `gitlab.*`, …) keep historical behaviour — unknown fields return `(not set)` with exit 0 so pre-existing scripts don't break. Only `proxy.*` paths were tightened.

Type-specific rules enforced by `devboy config set`:

| Field                               | Validation                                          |
|------------------------------------|-----------------------------------------------------|
| `proxy.routing.strategy`            | enum (case-insensitive): `remote` / `local` / `local-first` / `remote-first` |
| `proxy.routing.fallback_on_error`   | bool — `true`/`false`, `1`/`0`, `yes`/`no`, `on`/`off` (case-insensitive) |
| `proxy.secrets.cache_ttl_secs`      | non-negative integer (`0` disables cache)           |
| `proxy.telemetry.enabled`           | same bool forms as above                            |
| `proxy.telemetry.endpoint`          | URL beginning with `http://` or `https://`, non-empty host, no whitespace. Empty string clears the field. |
| `proxy.telemetry.token_key`         | arbitrary string; empty clears                      |
| `proxy.telemetry.batch_size`        | non-negative integer                                |
| `proxy.telemetry.batch_interval_secs` | non-negative integer                              |
| `proxy.telemetry.offline_queue_max` | non-negative integer                                |

Negative integers (`-1`) are accepted by the CLI argument parser (`allow_hyphen_values = true`) and rejected by the domain validator with a clear message.

### Telemetry endpoint payload

The backend enforces a strict shape on the POST body so malformed events don't create garbage rows in `mcp_tool_usages`:

| Field              | Rule                                                              |
|--------------------|-------------------------------------------------------------------|
| `events`           | Array, 1–1000 items (empty → 400, >1000 → 400)                    |
| `tool`             | Required; matches `^[a-z][a-z0-9_]*$` (lowercase + digits + `_`); ≤128 chars |
| `routing_decision` | Required; ≤64 chars                                               |
| `routing_detail`   | Optional; ≤256 chars                                              |
| `upstream`         | Optional; ≤64 chars                                               |
| `status`           | `"success"` or `"error"`                                          |
| `latency_ms`       | Non-negative integer                                              |
| `timestamp_secs`   | Non-negative integer ≤ `4102444800` (2100-01-01 UTC)              |
| `was_fallback`     | Optional boolean                                                  |

On any validation failure the whole batch is rejected (`400 Bad Request`) — no partial acceptance. Clients should retry after fixing the payload.

### MCP protocol and stdout hygiene

All commands send logs to **stderr**, not stdout. stdout stays clean for:

- JSON-RPC messages when running `devboy mcp`
- Machine-readable output (`devboy proxy status --json`, `devboy config get …`)
- Any output you pipe into `jq`, `python`, or another MCP client

Use `RUST_LOG=devboy_mcp::routing=info devboy mcp 2> routing.log` to capture routing decisions without polluting the JSON-RPC channel.
