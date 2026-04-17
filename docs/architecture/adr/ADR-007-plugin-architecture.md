---
id: ADR-007
title: Plugin architecture — API plugins, pipeline plugins, and capability model
status: accepted
date: 2026-01-13
deciders: ["Andrei Mazniak"]
tags: ["rust", "plugins", "architecture", "pipeline"]
supersedes: null
superseded_by: null
---

# ADR-007: Plugin architecture

## Status

**accepted** (Parts 1–2). Parts 3–4 (WASM Component Model, TypeScript plugin SDK) are kept as future work but not implemented.

- Part 1 — API and Pipeline plugin split via Cargo workspace: **shipped**
- Part 2 — `ExtensionSlots` with `TypeId`-based type-safe access, feature flags, per-plugin Cargo dependencies: **shipped**
- Part 3 — WASM Component Model for community plugins: **future work, deferred**
- Part 4 — TypeScript plugin SDK via child-process JSON-RPC: **future work, deferred**

## Context

`devboy-tools` needs an extensible architecture for three groups of add-ons:

1. **API plugins** — provider integrations (GitLab, GitHub, ClickUp, Jira, Slack, Fireflies, future: Linear, Sentry, …)
2. **Pipeline plugins** — output processing (pagination, truncation, summary, enrichment)
3. **Community plugins** — third-party extensions distributed outside the main repository

Key requirements:

- Typed data flow between plugins (a downstream plugin can use a typed value produced by an upstream plugin)
- Feature flags for optional plugins so lean builds are possible
- Compile-time verification for plugins shipped inside the binary, runtime verification for community plugins

## Decision

### Part 1: Two plugin types

```
┌────────────────────────────────────────────────────────────┐
│                        Plugin Types                         │
├────────────────────────────────────────────────────────────┤
│                                                             │
│  API PLUGINS                      PIPELINE PLUGINS          │
│  ───────────                      ────────────────          │
│                                                             │
│  + Credentials config             + Input schema extension  │
│  + API client                     + Output transformation   │
│  + Provider trait impl            + Guidance generation     │
│  + Entity mapping                 + Context enrichment      │
│                                                             │
│  crates/plugins/api/gitlab        crates/plugins/pipeline/ │
│  crates/plugins/api/github          paginate                │
│  crates/plugins/api/clickup         truncate                │
│  crates/plugins/api/jira            summary                 │
│  crates/plugins/api/slack           enrich                  │
│  crates/plugins/api/fireflies                               │
│                                                             │
└────────────────────────────────────────────────────────────┘
```

API plugins implement the provider traits from ADR-004. Pipeline plugins implement `PipelinePlugin` and operate on the `PipelineContext` that carries a tool call's input, output, and cross-plugin extensions.

### Part 2: `ExtensionSlots` with `TypeId`-based access

Rust's `TypeId` gives us a type-safe but extensible slot map: each plugin can publish a typed extension and downstream plugins can pull it out by type.

```rust
// crates/devboy-core/src/pipeline/context.rs
pub struct ExtensionSlots {
    slots: HashMap<TypeId, Box<dyn Any + Send + Sync>>,
}

impl ExtensionSlots {
    pub fn get<T: 'static + Send + Sync>(&self) -> Option<&T> {
        self.slots.get(&TypeId::of::<T>())
            .and_then(|b| b.downcast_ref::<T>())
    }
    pub fn set<T: 'static + Send + Sync>(&mut self, ext: T) {
        self.slots.insert(TypeId::of::<T>(), Box::new(ext));
    }
}

pub struct PipelineContext {
    pub input: Value,
    pub output: Value,
    pub extensions: ExtensionSlots,
    pub guidance: LlmGuidance,
    pub metadata: ContextMetadata,
}
```

#### Typed cross-plugin dependencies via Cargo

A pipeline plugin that needs the pagination extension declares a regular Cargo dependency:

```toml
# crates/plugins/pipeline/summary/Cargo.toml
[dependencies]
devboy-core           = { workspace = true }
devboy-pipeline-paginate = { workspace = true }
```

And imports the extension type directly:

```rust
// crates/plugins/pipeline/summary/src/lib.rs
use devboy_pipeline_paginate::PaginateExtension;

impl PipelinePlugin for SummaryPlugin {
    fn process(&self, ctx: &mut PipelineContext) -> Result<()> {
        if let Some(pag) = ctx.extensions.get::<PaginateExtension>() {
            ctx.guidance.add_hint(format!(
                "Showing page {} of {}",
                pag.current_page,
                pag.total_pages.unwrap_or(1),
            ));
        }
        Ok(())
    }
}
```

This gives us IDE autocomplete, go-to-definition, compile-time type checking, and explicit semver-versioned dependencies — all for free, just by using Cargo.

#### Feature flags

```toml
# crates/devboy-core/Cargo.toml
[features]
default = ["all-pipeline"]

all-pipeline = [
    "pipeline-paginate", "pipeline-truncate",
    "pipeline-summary",  "pipeline-enrich",
]

pipeline-paginate = ["devboy-pipeline-paginate"]
pipeline-truncate = ["devboy-pipeline-truncate"]
pipeline-summary  = ["devboy-pipeline-summary", "pipeline-paginate", "pipeline-truncate"]
pipeline-enrich   = ["devboy-pipeline-enrich"]

all-providers = [
    "gitlab", "github", "clickup", "jira", "slack", "fireflies",
]
```

A CI job builds a feature matrix to confirm every combination compiles and passes its tests.

### Part 2b: Capability system

Each plugin declares what it **provides** and what it **requires** via a `Capability` value. Core capabilities are a Rust enum (compile-time typed); custom capabilities are strings with a `namespace:action` shape, so community plugins can add their own without core changes.

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CoreCapability {
    IssueRead, IssueWrite, IssueDelete, IssueComment, IssueRelations, IssueAttachment,
    MergeRequestRead, MergeRequestWrite, MergeRequestDiscussion, MergeRequestPipeline,
    ChatRead, ChatWrite, ChatSearch,
    MeetingRead, MeetingTranscript,
    Pagination, Truncation, Summarization, Enrichment, GuidanceGeneration,
}

pub enum Capability {
    Core(CoreCapability),
    Custom(String),   // "linear:team:read", "jira:agile:*"
}
```

A `CapabilitySet` with wildcard matching (`issue:*`, `linear:**`) allows both strict and permissive requirements.

### Part 3 (future work, deferred): WASM Component Model for community plugins

When community plugins become a priority, we intend to use the W3C WASM Component Model:

- WIT interface definitions for plugin/host contracts
- `wasmtime` Component Model for the host runtime
- Host-supplied imports: HTTP, logging, credential lookup
- Sandboxed execution — community plugins never touch the host filesystem or network except through host functions

A sketch of the WIT surface and the `wasmtime` host integration exists in design notes but has not been implemented. Until we have a concrete community-plugin use case, this stays deferred.

### Part 4 (future work, deferred): TypeScript plugin SDK

Another deferred path is a child-process TypeScript SDK:

- Plugins run as a Node.js process
- Communicate with the host over JSON-RPC on stdio
- `@devboy-tools/plugin-sdk` npm package provides the SDK and plugin lifecycle helpers
- Full access to the NPM ecosystem (e.g. `@linear/sdk` for a Linear plugin)

Rationale for deferring: we currently have no confirmed use case that requires running user-authored TypeScript inside the binary. The Rust-crate path covers every provider we've built so far.

### Verification

#### Compile-time (core plugins)

Core plugins are verified at compile time: they use trait bounds and macros to assert API version compatibility and capability consistency.

```rust
pub trait VerifiedPlugin: PipelinePlugin {
    const API_VERSION: ApiVersion;
    const PROVIDES: &'static [Capability];
    const REQUIRES: &'static [Capability];
}

#[macro_export]
macro_rules! verify_plugin_deps {
    ($plugin:ty) => {
        const _: () = {
            let api = <$plugin as VerifiedPlugin>::API_VERSION;
            assert!(api.major == $crate::CORE_API_VERSION.major);
        };
    };
}
```

#### Runtime (community plugins, when they land)

For community plugins we intend to ship a `PluginVerifier` that checks a `PluginManifest` at load time — version compatibility, dependency resolution, schema validity, capability consistency, and a conformance test suite run against the loaded plugin in a sandbox. This is all in the "future work" bucket alongside Parts 3–4.

### CI

```yaml
# .github/workflows/plugin-verification.yml
jobs:
  feature-matrix:
    strategy:
      matrix:
        features:
          - default
          - pipeline-paginate
          - pipeline-paginate,pipeline-truncate
          - pipeline-paginate,pipeline-truncate,pipeline-summary
          - all-pipeline
          - all-pipeline,all-providers
    steps:
      - run: cargo test --no-default-features --features "${{ matrix.features }}"
```

## Consequences

### Positive

- ✅ **Type safety** — `ctx.extensions.get::<PaginateExtension>()` is compile-time checked
- ✅ **IDE support** — autocomplete and go-to-definition across plugin boundaries
- ✅ **Explicit dependencies** — each plugin's `Cargo.toml` lists exactly what it depends on
- ✅ **Semver** — plugins version with the rest of the workspace
- ✅ **Zero cost when unused** — `Option<&T>` is as cheap as a pointer check
- ✅ **Feature-matrix CI** — prevents accidentally coupling plugins that shouldn't be coupled

### Negative

- ❌ **Complexity** — two verification strategies (compile-time for core, runtime for community once shipped)
- ❌ **Boilerplate** — each plugin needs a `VerifiedPlugin` impl with its capabilities
- ❌ **CI build time** — the feature matrix multiplies the number of `cargo test` invocations

### Trade-offs

| Aspect | Core plugins | Community plugins (future) |
|--------|--------------|-----------------------------|
| Verification | Compile-time | Runtime |
| Trust model | Trusted — shipped inside the binary | Sandboxed via WASM |
| Update path | Recompile the binary | Hot reload at runtime |
| Dependency declaration | `Cargo.toml` | `manifest.json` |

## Alternatives Considered

### Alternative 1: `HashMap<String, Value>` for extensions

**Why rejected:** No type safety, no IDE help, every downstream read becomes a string key plus a `serde_json::from_value` call that can fail at runtime.

### Alternative 2: Enum for known extensions

```rust
pub enum Extension {
    Pagination(PaginateExtension),
    Truncation(TruncateExtension),
    // ...
}
```

**Why rejected:** Adding a new extension means changing the core enum — defeats the plugin split.

### Alternative 3: Only runtime verification (no compile-time for core)

**Why rejected:** Core plugins ship inside the binary. It would be silly to defer their validation to startup when Rust can prove it at build time.

## Implementation

- **Shared types:** `crates/devboy-core/src/pipeline/` (context, extensions, capabilities)
- **API plugins:** `crates/plugins/api/<provider>/`
- **Pipeline plugins:** `crates/plugins/format-pipeline/`
- **Feature flags:** `crates/devboy-core/Cargo.toml`
- **Feature-matrix CI:** `.github/workflows/` (feature permutations per job)

## References

- [`std::any::TypeId`](https://doc.rust-lang.org/std/any/struct.TypeId.html)
- [Cargo features](https://doc.rust-lang.org/cargo/reference/features.html)
- [WASM Component Model](https://component-model.bytecodealliance.org/) — deferred (Part 3)
- [WIT specification](https://github.com/WebAssembly/component-model/blob/main/design/mvp/WIT.md) — deferred (Part 3)
- [ADR-004: Trait-based provider abstraction](./ADR-004-trait-based-mocking.md)

---

## Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-01-13 | Claude Code | Initial version |
| 2026-04-17 | Claude Code | Translated to English; split into shipped (Parts 1–2) and deferred (Parts 3–4) sections; trimmed in-doc code; marked accepted for the shipped subset |
