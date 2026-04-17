---
id: ADR-002
title: Rust-based architecture with npm binary distribution
status: accepted
date: 2026-01-12
deciders: ["Andrei Mazniak"]
tags: ["architecture", "rust", "distribution"]
supersedes: null
superseded_by: null
---

# ADR-002: Rust-based architecture with npm binary distribution

## Status

**accepted** — implemented. The Rust workspace, CLI, MCP server, provider plugins, and npm wrapper are all shipped.

## Context

`devboy-tools` is a configurable bundle of integration tools for AI coding agents. It needs to:

1. Install easily on a developer machine without requiring a language runtime
2. Run on macOS, Linux, and Windows
3. Ship a single self-contained binary so that `npx devboy` works out of the box
4. Stay fast enough that loading tools into an agent's context is cheap
5. Allow multiple transport surfaces over the same tool set (MCP over stdio, CLI commands, etc.)

## Decision

> **Decision:** Implement the tool bundle as a **Rust** Cargo workspace and distribute the compiled binary through an **npm wrapper** with platform-specific sub-packages.

### Workspace layout

```
devboy-tools/
├── Cargo.toml                     # Workspace root
├── LICENSE                        # Apache 2.0
├── README.md
├── crates/
│   ├── devboy-core/               # Traits (Provider, ToolEnricher), shared types, config
│   ├── devboy-executor/           # Tool execution engine + enrichment pipeline
│   ├── devboy-mcp/                # MCP server (JSON-RPC over stdio)
│   ├── devboy-cli/                # CLI binary (entry point)
│   ├── devboy-storage/            # Credential storage (keychain, env vars)
│   └── plugins/
│       ├── api/                   # Provider integrations
│       │   ├── gitlab/
│       │   ├── github/
│       │   ├── clickup/
│       │   ├── jira/
│       │   ├── slack/
│       │   └── fireflies/
│       └── format-pipeline/       # Output formatting (TOON, markdown, budget trimming)
├── npm/                           # npm distribution wrappers
│   ├── devboy-tools/              # Main `@devboy-tools/cli` package
│   └── devboy-tools-{platform}/   # Per-platform binaries
└── docs/                          # User docs (Rspress)
```

### Distribution

- **Primary channel:** `npm install -g @devboy-tools/cli` (or `pnpm add -g`). The wrapper selects the correct platform-specific binary at install time.
- **Alternative:** release binaries from GitHub Releases, or build from source via `cargo install`.

### Transports

The same tool set is exposed through multiple transports (see the README's "Integration modes" section):

- **MCP server** — `devboy mcp` over stdio for Claude Code, Claude Desktop, and any MCP client
- **CLI** — subcommands like `devboy issues`, `devboy mrs`, `devboy tools call <name>` for humans, CI jobs, and agent skills
- Agent skills that avoid the full MCP tool-list tax by invoking single tools via `devboy tools call`

## Consequences

### Positive

- ✅ Single self-contained binary — no Node.js runtime or language-specific package manager required on the target machine
- ✅ Cross-platform coverage via cross-compilation in CI
- ✅ `npx devboy` / global install for trivial onboarding
- ✅ Performance is acceptable for interactive agent use — tool calls and response shaping are measured in milliseconds
- ✅ Clean separation between transport (CLI, MCP) and tool logic (`devboy-executor`, providers)

### Negative

- ❌ Rust has a steeper learning curve than TypeScript for contributors arriving from web backgrounds
- ❌ More moving parts in release: cross-compilation matrix, per-platform npm packages, code-signing on macOS/Windows
- ❌ Some providers benefit from rich existing SDKs (e.g. GitHub Octokit); we re-implement the needed surface in Rust

### Risks

- ⚠️ **Time to re-implement provider surface in Rust** — mitigation: start with the minimum set of methods the agent actually needs, grow incrementally
- ⚠️ **Release tooling complexity** — mitigation: keep the cross-compile matrix in CI; invest in solid release automation early
- ⚠️ **Contributor ramp-up** — mitigation: `CONTRIBUTING.md` covers the workspace layout and test patterns; the plugin architecture (see ADR-007) means many contributors can work on a single provider crate without touching core

## Alternatives Considered

### Alternative 1: Stay on a TypeScript/Node.js implementation

**Description:** Ship as a TypeScript npm package running on the user's Node.js runtime.

**Why rejected:**

- Requires a Node.js runtime on the target machine
- Startup latency for MCP stdio servers is worse
- Dependency trees can conflict with the user's own Node.js projects when installed globally
- Binary distribution is awkward (`pkg`/`nexe` solve this but introduce their own set of problems)

### Alternative 2: Go

**Description:** Rewrite in Go — also compiles to a single binary.

**Why rejected:** Rust gives stronger compile-time guarantees around the plugin trait boundaries we want (see ADR-004, ADR-007). The Rust MCP SDK ecosystem is active enough to commit to. Go is perfectly viable but doesn't add value over Rust for this project's constraints.

### Alternative 3: Electron / Tauri for a desktop UI

**Description:** Ship a desktop app instead of a CLI + MCP server.

**Why rejected:** CLI plus MCP stdio covers the target use cases (agents and CI). A desktop UI is a separate product concern and can be added later without breaking this architecture.

## Implementation

- **Workspace:** `Cargo.toml` at repository root, member crates under `crates/`
- **Binary entry point:** `crates/devboy-cli/src/main.rs`
- **npm wrapper:** `npm/devboy-tools/`, published as `@devboy-tools/cli`
- **CI:** cross-compilation matrix (macOS arm64/x64, Linux arm64/x64, Windows x64)

## References

- [MCP Specification](https://spec.modelcontextprotocol.io/)
- [Rust MCP SDK](https://github.com/modelcontextprotocol/rust-sdk)
- [ADR-001: Apache 2.0 License](./ADR-001-apache-license.md)
- [ADR-004: Trait-based mocking for the provider abstraction](./ADR-004-trait-based-mocking.md)
- [ADR-007: Plugin architecture](./ADR-007-plugin-architecture.md)

---

## Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-01-12 | Claude Code | Initial version |
| 2026-04-17 | Claude Code | Translated to English; trimmed to the scope of this project; marked as accepted (implementation shipped) |
