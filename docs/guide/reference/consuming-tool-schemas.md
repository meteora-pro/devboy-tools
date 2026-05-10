# Consuming tool schemas

The Markdown reference at [`tools.md`](./tools.md) is auto-generated from the same source the runtime serves to MCP clients. If you're building tooling around devboy — code generators, IDE plugins, alternative MCP surfaces, documentation sites — you want the **machine-readable** version of those schemas, not a Markdown parser.

This page shows how to pull them out of the published [`devboy-executor`](https://crates.io/crates/devboy-executor) crate.

## Cargo dependency

`devboy-executor` is published on crates.io as part of the workspace release ([ADR-022](../architecture/adr/ADR-022-crates-io-publishing.md)). Pin a minor version — the public surface follows SemVer:

```toml
[dependencies]
devboy-executor = "0.28"
```

You don't need the rest of the workspace just to read schemas. The transitive `devboy-core` dep gives you the `ToolCategory` enum.

## Listing every tool

`base_tool_definitions()` returns a `Vec<ToolDefinition>` — every provider-backed built-in tool, in declaration order. No I/O, no async, allocation-free across calls.

```rust
use devboy_executor::tools::base_tool_definitions;

let tools = base_tool_definitions();
for t in &tools {
    println!("{} ({:?})  {}", t.name, t.category, t.description);
}
```

`ToolDefinition` itself is a small POD:

```rust
pub struct ToolDefinition {
    pub name: String,            // e.g. "create_issue"
    pub description: String,
    pub category: ToolCategory,  // IssueTracker | GitRepository | ...
    pub input_schema: ToolSchema,
}
```

The schema is a structured form, not a raw JSON Schema string — see `devboy_core::ToolSchema` and `PropertySchema` for the model. Serialising it back out is a one-liner if your downstream needs JSON Schema:

```rust
let json: serde_json::Value = serde_json::to_value(&t.input_schema)?;
```

## Filtering by category

Each `ToolDefinition` carries a `ToolCategory`. Categories come from `devboy_core`:

```rust
use devboy_core::ToolCategory;
use devboy_executor::tools::base_tool_definitions;

let issue_tracker_tools: Vec<_> = base_tool_definitions()
    .into_iter()
    .filter(|t| t.category == ToolCategory::IssueTracker)
    .collect();
```

Other categories: `GitRepository`, `MeetingNotes`, `KnowledgeBase`, `Messenger`, `JiraStructure`. The full list is on [Tool reference → Provider Support Matrix](./tools.md#provider-support-matrix).

## Per-provider augmentation

The base list is provider-agnostic. Some categories are conditionally available depending on what's configured at runtime — the runtime augments the list via `Provider::tools()` before serving `tools/list` to MCP clients. If you're building something that mirrors that runtime view (rather than the static catalogue), wire the same pattern: start from `base_tool_definitions()`, then merge per-provider tools.

## Stability guarantees

`base_tool_definitions()` is part of the published `devboy-executor` API. Per [ADR-022](../architecture/adr/ADR-022-crates-io-publishing.md):

- **Tool name and category** never change without a major-version bump.
- **Tool descriptions and schema field descriptions** may evolve in patch releases — they're prose for LLMs.
- **Schema shape** (which fields exist, their types, required-ness) only changes in minor releases, additively.

If you're caching the schemas at build time, refresh on minor bumps; if you're reading at runtime, you're always in sync.

## Use cases this enables

- **Code generators** that emit typed clients per language from `input_schema`.
- **IDE plugins** that surface tool descriptions and parameter hints inline.
- **Alternative MCP surfaces** (HTTP, gRPC, in-process) that need the same catalogue without re-implementing it.
- **Documentation sites** that render their own version of [`tools.md`](./tools.md) with custom styling, search, or interactivity.
- **Test harnesses** that generate fixtures from the schema instead of hand-writing payloads.

## See also

- [Tool reference](./tools.md) — human-readable Markdown for the same data.
- [Architecture → Executor](../architecture/executor.md) — how tool execution is structured.
- [ADR-022](../architecture/adr/ADR-022-crates-io-publishing.md) — what's published and why.
