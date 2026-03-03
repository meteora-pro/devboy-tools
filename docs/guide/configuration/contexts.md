# Contexts (Multi-Project)

Contexts allow you to manage multiple project configurations in a single config file and switch between them. This is especially useful for AI agents that work across several repositories.

## Why contexts?

Without contexts, the architecture follows a **1 server = 1 project** model. If you work on multiple projects, you'd need to restart the MCP server or maintain separate config files. Contexts solve this by letting you define multiple project profiles and switch between them.

## Defining contexts

Each context is a named group of provider configurations under the `[contexts.<name>]` section:

```toml
[contexts.devboy-tools.github]
owner = "meteora-pro"
repo = "devboy-tools"

[contexts.dashboard.github]
owner = "meteora-pro"
repo = "dev-boy-monorepo"

[contexts.dashboard.clickup]
list_id = "abc123"
team_id = "xyz789"
```

A context can include any combination of providers (GitHub, GitLab, ClickUp, Jira).

## Switching contexts

### CLI

```bash
# List all contexts and see which is active
devboy context list

# Switch to a different context
devboy context use dashboard
```

### MCP tools

When running as an MCP server, the following tools are available:

| Tool | Description |
|------|-------------|
| `list_contexts` | List all configured contexts with active marker |
| `use_context` | Switch the active context by name |
| `get_current_context` | Show the currently active context and its providers |

AI agents can use these tools to discover available projects and switch between them mid-conversation.

## Token resolution

Tokens are resolved in the following order:

1. **Context-scoped token**: `contexts.<name>.<provider>.token` in keychain
2. **Global provider token**: `<provider>.token` in keychain (shared fallback)

```bash
# Set a context-specific token
devboy config set-secret contexts.dashboard.github.token <token>

# Or use a single global token shared across contexts
devboy config set-secret github.token <token>
```

For most setups, a single global token per provider is sufficient.

## Active context resolution

When no context is explicitly selected, DevBoy resolves the active context in this order:

1. The `active_context` field in the config (if it points to a valid context)
2. A context named `default` (explicit or legacy)
3. The first context alphabetically

## Backward compatibility

Existing single-project configs continue to work. Top-level provider sections are treated as an implicit `default` context:

```toml
# Legacy format — still supported
[github]
owner = "meteora-pro"
repo = "devboy-tools"
```

This is equivalent to:

```toml
[contexts.default.github]
owner = "meteora-pro"
repo = "devboy-tools"
```

If both exist, the explicit `[contexts.default]` takes precedence.

## Full example

```toml
# Active context selection
active_context = "devboy-tools"

[contexts.devboy-tools.github]
owner = "meteora-pro"
repo = "devboy-tools"

[contexts.dashboard.github]
owner = "meteora-pro"
repo = "dev-boy-monorepo"

[contexts.dashboard.gitlab]
url = "https://gitlab.example.com"
project_id = "42"

[contexts.dashboard.clickup]
list_id = "abc123"
team_id = "xyz789"

[contexts.client-project.jira]
url = "https://company.atlassian.net"
project_key = "PROJ"
email = "user@example.com"
```
