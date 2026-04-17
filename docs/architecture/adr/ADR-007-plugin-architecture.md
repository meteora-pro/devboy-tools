---
id: ADR-007
title: Plugin architecture — API providers, format pipeline, and the enricher model
status: accepted
date: 2026-01-13
deciders: ["Andrei Mazniak"]
tags: ["rust", "plugins", "architecture", "pipeline"]
supersedes: null
superseded_by: null
---

# ADR-007: Plugin architecture

## Status

**accepted** — the shipped design is described below. Two earlier variants considered during the original discussion (typed `ExtensionSlots` with a `Capability` enum, and community plugins via WASM / TypeScript) are kept as **alternatives considered** and **deferred future work** respectively — neither is implemented today.

## Context

`devboy-tools` needs an extensible architecture for three groups of add-ons:

1. **API plugins** — provider integrations (GitLab, GitHub, ClickUp, Jira, Slack, Fireflies, …)
2. **Pipeline plugins** — output processing (TOON encoding, budget trimming, pagination, truncation, …)
3. **Community plugins** — third-party extensions distributed outside the main repository *(deferred — no shipped mechanism yet)*

Key requirements:

- A provider crate must be addable without touching core
- Tools surfaced to the agent should adapt to what the active providers actually support (no empty shells of tools that always return "not supported")
- Core crates stay free of any provider-specific code

## Decision

### Part 1 — Two plugin types, two shapes

```
┌────────────────────────────────────────────────────────────────────┐
│                          Plugin types                               │
├────────────────────────────────────────────────────────────────────┤
│                                                                     │
│  API PLUGINS (one crate per provider)    FORMAT PIPELINE (one crate)│
│  ───────────────────────────────────     ───────────────────────────│
│                                                                     │
│  + Provider trait impl (ADR-004)          + TOON encoding           │
│  + API client                             + Budget trimming         │
│  + Entity mapping → core types            + Pagination              │
│  + ToolEnricher impl                      + Per-strategy trim logic │
│  + Category declarations                  + Output formatting       │
│                                                                     │
│  crates/plugins/api/gitlab                crates/plugins/           │
│  crates/plugins/api/github                      format-pipeline/    │
│  crates/plugins/api/clickup                     ├── budget/        │
│  crates/plugins/api/jira                        ├── pagination/    │
│  crates/plugins/api/slack                       ├── trim/          │
│  crates/plugins/api/fireflies                   ├── truncation/    │
│                                                 ├── toon/          │
│                                                 └── strategy/      │
│                                                                     │
└────────────────────────────────────────────────────────────────────┘
```

**API plugins are one crate per provider.** Each one implements the relevant provider traits from ADR-004 (`IssueProvider`, `MergeRequestProvider`, etc.), plus the `ToolEnricher` trait described below.

**The format pipeline is one crate (`devboy-format-pipeline`) with modules.** The original discussion considered splitting each pipeline stage (pagination, truncation, summarisation, enrichment) into its own crate; in practice the stages are tightly coupled through shared types (`TransformOutput`, `BudgetConfig`, `StrategyResolver`) and splitting them added Cargo surface without removing any real coupling. One crate, several modules turned out to be the right trade-off.

### Part 2 — Tool enrichment via `ToolEnricher` + `ToolCategory`

The capability model is expressed through two cooperating constructs, both in `devboy-core`:

```rust
// crates/devboy-core/src/tool_category.rs
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ToolCategory {
    GitRepository,   // MR/PR, pipelines, diffs, discussions
    IssueTracker,    // issues CRUD, comments, statuses
    Epics,           // high-level tasks with goals and progress
    Releases,        // tags, releases, release assets
    MeetingNotes,    // transcripts, summaries, search
    Messenger,       // chats, messages, search, sending
}

// crates/devboy-core/src/enricher.rs
pub trait ToolEnricher: Send + Sync {
    /// Which categories this provider serves. Tools from categories
    /// not covered by any registered enricher are hidden in `tools/list`.
    fn supported_categories(&self) -> &[ToolCategory];

    /// Adapt the JSON Schema of a tool at `tools/list` time (add provider-
    /// specific parameters, set enum values from real project metadata,
    /// strip unsupported fields).
    fn enrich_schema(&self, tool_name: &str, schema: &mut ToolSchema);

    /// Transform arguments before execution (e.g. `cf_story_points`
    /// → `customFields.storyPoints` for ClickUp).
    fn transform_args(&self, tool_name: &str, args: &mut Value);
}
```

The model is deliberately **provider-scoped**, not fine-grained capability-scoped:

- A provider declares a handful of **categories** it serves. The executor filters the advertised tools accordingly, so an agent talking to an MCP server configured with only Slack + Fireflies does not see `create_issue` or `get_merge_requests` in its tool list.
- Inside a category, the provider **enriches** the tool schema to match what it can really do — injecting custom-field enums, removing params it cannot honour, adding free-form provider-specific parameters.

This gives us most of what a richer capability system would give us (tools that don't fit are hidden; those that do fit get schemas that match reality), without the cost of maintaining a large `Capability` enum or a runtime verification layer.

### Part 3 — Community plugins *(deferred)*

The original discussion also sketched two mechanisms for third-party plugins:

- **WASM Component Model** with WIT interfaces, `wasmtime` host, and sandboxed HTTP / credentials / logging imports
- **TypeScript plugin SDK** via a child Node.js process speaking JSON-RPC over stdio, published as `@devboy-tools/plugin-sdk`

Neither is implemented and neither is in the current scope. They are kept here as a reminder that the core design doesn't preclude them — the `Provider` + `ToolEnricher` split is the natural seam to plug a WASM or child-process runtime into when the need becomes concrete.

## Consequences

### Positive

- ✅ **New provider = new crate.** No core changes are required to add a provider; Cargo workspace members + `devboy_core` trait impls cover it.
- ✅ **Tool list matches provider set.** Agents only see tools that can actually run against the active configuration.
- ✅ **Schemas reflect reality.** Provider-specific parameters and enums come from the enricher, not from static MCP declarations.
- ✅ **Simple to reason about.** Six categories, one trait — no type-map of extension slots, no runtime capability resolver.
- ✅ **Format pipeline is one place.** Truncation, TOON encoding, budget trimming, and pagination share types and code without the friction of several crates.

### Negative

- ❌ Category granularity is coarser than per-capability gating. If two providers belong to the same category but support different subsets of its tools, we can't currently hide individual tools for one of them — we rely on the enricher to communicate that through the schema (e.g. restricted enum values).
- ❌ The format pipeline being one crate means a consumer can't trivially pull in just `truncation` without the rest. No feature flags for per-module selection today. Not currently a pain point, but it is a trade-off.

## Alternatives Considered

### Alternative 1: Type-safe `ExtensionSlots` + `Capability` enum

**Sketch (from the original discussion):**

- `ExtensionSlots` backed by `HashMap<TypeId, Box<dyn Any + Send + Sync>>`, giving downstream plugins typed access (`ctx.extensions.get::<PaginateExtension>()`) to upstream plugin state.
- A `CoreCapability` enum enumerating every operation (`IssueRead`, `IssueWrite`, `MergeRequestDiscussion`, `Pagination`, …) with a `Custom(String)` escape hatch, plus a `CapabilitySet` with wildcard matching (`issue:*`, `linear:**`).
- A `VerifiedPlugin` trait with `PROVIDES` / `REQUIRES` constants, plus a compile-time macro asserting API-version compatibility.

**Why not adopted:** in practice, the cross-plugin dependencies we actually need are already typed — a pipeline stage that depends on another's output shares a module in the same crate and uses its types directly, no `TypeId` lookup required. The fine-grained capability enum would have needed constant maintenance as the tool surface evolved, and the wildcard resolver introduced a runtime layer for problems that were better solved at the category level. `ToolEnricher` + `ToolCategory` delivers the same "advertise what you can really do" property for less ongoing cost.

### Alternative 2: Per-pipeline-stage Cargo crates with feature flags

**Sketch:** `devboy-pipeline-paginate`, `devboy-pipeline-truncate`, `devboy-pipeline-summary`, `devboy-pipeline-enrich`, each its own crate; `devboy-core` pulls them in via `optional = true` dependencies gated by `pipeline-paginate` / `pipeline-truncate` / … features; a feature matrix in CI verifies every combination.

**Why not adopted:** the stages share substantial data and control flow (`TransformOutput`, `StrategyResolver`, budget bookkeeping). Splitting them into separate crates would have duplicated types or forced a shared "-types" crate that defeats the purpose of the split. One crate with well-factored modules is cleaner and we can revisit if a real need emerges to drop individual stages from a lean build.

### Alternative 3: Runtime plugin registry with dynamic loading

**Why not adopted:** static Cargo membership keeps the build reproducible, gets us type safety across plugin boundaries for free, and avoids the operational complexity of discovering and verifying shared libraries at startup. Dynamic loading can be added through the deferred community-plugin track (WASM / child-process) without changing the core design.

## Implementation

- **Traits:** `crates/devboy-core/src/provider.rs`, `crates/devboy-core/src/enricher.rs`, `crates/devboy-core/src/tool_category.rs`
- **API plugins:** `crates/plugins/api/{gitlab,github,clickup,jira,slack,fireflies}/`
- **Format pipeline:** `crates/plugins/format-pipeline/` with modules `budget`, `pagination`, `strategy`, `token_counter`, `toon`, `tree`, `trim`, `truncation`
- **Executor wiring:** `crates/devboy-executor/src/` registers enrichers per provider and filters tools by category

## References

- [ADR-002: Rust-based architecture](./ADR-002-rust-architecture.md)
- [ADR-004: Trait-based provider abstraction](./ADR-004-trait-based-mocking.md)
- [MCP `tools/list` specification](https://spec.modelcontextprotocol.io/)

---

## Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-01-13 | Andrei Mazniak | Initial version — proposed `ExtensionSlots` + `Capability` enum + WASM/TS community plugins |
| 2026-04-17 | Andrei Mazniak | Rewritten to match the shipped design: `ToolEnricher` + `ToolCategory` (six categories) for provider-scoped enrichment; format pipeline kept as a single crate with modules; original typed-capability and community-plugin sketches moved into Alternatives and Deferred sections |
