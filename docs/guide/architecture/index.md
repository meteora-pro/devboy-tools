# Architecture

DevBoy tools is organized as a Cargo workspace with clear separation of concerns.

## Crate Layout

```
crates/
├── devboy-core/          # Traits, types, config, error handling
├── devboy-executor/      # Tool execution engine + enrichment pipeline
├── devboy-mcp/           # MCP server (JSON-RPC over stdio)
├── devboy-cli/           # CLI binary
├── devboy-storage/       # Credential storage (keychain, env vars)
└── plugins/
    ├── api/              # Provider integrations
    │   ├── gitlab/       # Client + enricher + types
    │   ├── github/       # Client + enricher + types
    │   ├── clickup/      # Client + enricher + metadata + types
    │   └── jira/         # Client + enricher + metadata + types
    └── format-pipeline/  # TOON encoding, budget trimming, pagination
```

## Dependency Graph

```
devboy-core (traits: Provider, ToolEnricher, types)
    ▲
    ├── plugins/api/* (provider clients + enrichers)
    │
devboy-executor (Executor, factory, format, ProviderConfig)
    ▲
    ├── plugins/pipeline (output formatting)
    │
    ├── devboy-mcp (MCP stdio transport)
    └── devboy-cli (CLI commands)
```

Key principle: `devboy-executor` does NOT depend on `devboy-storage`. Credentials come via `AdditionalContext` from the caller (CLI resolves from config + keychain, cloud resolves from database).

## Request Flow

```
Client (AI assistant)
    │
    ▼
MCP Server (devboy-mcp) ← JSON-RPC over stdio
    │
    ├── Context management (list_contexts, use_context)
    ├── Proxy tools (forwarded to upstream MCP servers)
    │
    ▼
Tool Handler
    │
    ├── Provider creation (from config + keychain)
    ├── Tool dispatch (get_issues, get_merge_requests, ...)
    ├── Pipeline formatting (markdown/compact/json)
    │
    ▼
Provider (GitLab/GitHub/ClickUp/Jira API calls)
```

## Executor Flow (for NAPI/HTTP consumers)

```
Caller (NAPI bridge, HTTP server)
    │
    ▼
Executor
    │
    1. Enrichers transform args (cf_* → customFields)
    2. Factory creates provider from ProviderConfig
    3. Provider executes API calls → typed ToolOutput
    4. Format: Pipeline converts ToolOutput → text
    │
    ▼
Result (formatted text)
```

See [Executor](./executor) and [Enrichers](./enrichers) for details.
