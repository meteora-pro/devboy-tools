# DevBoy tools

[![CI](https://github.com/meteora-pro/devboy-tools/actions/workflows/ci.yml/badge.svg)](https://github.com/meteora-pro/devboy-tools/actions/workflows/ci.yml)
[![Codecov](https://codecov.io/gh/meteora-pro/devboy-tools/branch/main/graph/badge.svg)](https://codecov.io/gh/meteora-pro/devboy-tools)
[![License](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](https://opensource.org/licenses/Apache-2.0)
[![zread](https://img.shields.io/badge/Ask_Zread-_.svg?style=flat&color=00b0aa&labelColor=000000&logo=data%3Aimage%2Fsvg%2Bxml%3Bbase64%2CPHN2ZyB3aWR0aD0iMTYiIGhlaWdodD0iMTYiIHZpZXdCb3g9IjAgMCAxNiAxNiIgZmlsbD0ibm9uZSIgeG1sbnM9Imh0dHA6Ly93d3cudzMub3JnLzIwMDAvc3ZnIj4KPHBhdGggZD0iTTQuOTYxNTYgMS42MDAxSDIuMjQxNTZDMS44ODgxIDEuNjAwMSAxLjYwMTU2IDEuODg2NjQgMS42MDE1NiAyLjI0MDFWNC45NjAxQzEuNjAxNTYgNS4zMTM1NiAxLjg4ODEgNS42MDAxIDIuMjQxNTYgNS42MDAxSDQuOTYxNTZDNS4zMTUwMiA1LjYwMDEgNS42MDE1NiA1LjMxMzU2IDUuNjAxNTYgNC45NjAxVjIuMjQwMUM1LjYwMTU2IDEuODg2NjQgNS4zMTUwMiAxLjYwMDEgNC45NjE1NiAxLjYwMDFaIiBmaWxsPSIjZmZmIi8%2BCjxwYXRoIGQ9Ik00Ljk2MTU2IDEwLjM5OTlIMi4yNDE1NkMxLjg4ODEgMTAuMzk5OSAxLjYwMTU2IDEwLjY4NjQgMS42MDE1NiAxMS4wMzk5VjEzLjc1OTlDMS42MDE1NiAxNC4xMTM0IDEuODg4MSAxNC4zOTk5IDIuMjQxNTYgMTQuMzk5OUg0Ljk2MTU2QzUuMzE1MDIgMTQuMzk5OSA1LjYwMTU2IDE0LjExMzQgNS42MDE1NiAxMy43NTk5VjExLjAzOTlDNS42MDE1NiAxMC42ODY0IDUuMzE1MDIgMTAuMzk5OSA0Ljk2MTU2IDEwLjM5OTlaIiBmaWxsPSIjZmZmIi8%2BCjxwYXRoIGQ9Ik0xMy43NTg0IDEuNjAwMUgxMS4wMzg0QzEwLjY4NSAxLjYwMDEgMTAuMzk4NCAxLjg4NjY0IDEwLjM5ODQgMi4yNDAxVjQuOTYwMUMxMC4zOTg0IDUuMzEzNTYgMTAuNjg1IDUuNjAwMSAxMS4wMzg0IDUuNjAwMUgxMy43NTg0QzE0LjExMTkgNS42MDAxIDE0LjM5ODQgNS4zMTM1NiAxNC4zOTg0IDQuOTYwMVYyLjI0MDFDMTQuMzk4NCAxLjg4NjY0IDE0LjExMTkgMS42MDAxIDEzLjc1ODQgMS42MDAxWiIgZmlsbD0iI2ZmZiIvPgo8cGF0aCBkPSJNNCAxMkwxMiA0TDQgMTJaIiBmaWxsPSIjZmZmIi8%2BCjxwYXRoIGQ9Ik00IDEyTDEyIDQiIHN0cm9rZT0iI2ZmZiIgc3Ryb2tlLXdpZHRoPSIxLjUiIHN0cm9rZS1saW5lY2FwPSJyb3VuZCIvPgo8L3N2Zz4K&logoColor=ffffff)](https://zread.ai/meteora-pro/devboy-tools)

A fast, Open Source **configurable tool bundle** for AI coding agents, written in Rust. DevBoy ships a curated set of dev-workflow tools (GitHub, GitLab, ClickUp, Jira, and more) that can be plugged into any agent three ways: as an **MCP server**, as a **CLI** for humans and scripts, or as **agent skills** that call individual tools directly. Under the hood: plugin system for API providers, an LLM-optimized output pipeline, and multi-project context switching.

## Integration modes

DevBoy is a tool bundle first — the transport is your choice:

| Mode | When to use | Example |
|------|-------------|---------|
| **MCP server** | Claude Desktop, Claude Code, any MCP-compatible client | `devboy mcp` (stdio) |
| **CLI** | Humans, CI jobs, shell scripts | `devboy issues`, `devboy mrs` |
| **Agent skills** | Agents that don't want the full MCP tool-list tax — call just the tools a skill needs | `devboy tools call get_issues` from a skill script |

The same tools, the same pipeline, three ways to reach them. Start with one mode and layer on the others as your workflow grows.

> **Note on JSON arguments.** `devboy tools call <name>` takes an optional positional JSON string (defaults to `{}`). On POSIX shells wrap it in single quotes: `devboy tools call get_issues '{"limit": 20}'`. On Windows `cmd.exe` / PowerShell escape the inner quotes instead: `devboy tools call get_issues "{\"limit\": 20}"`.

## Skills — procedural recipes shipped with the tools

`devboy-tools` ships a catalogue of **skills** — one-page Markdown recipes that tell an AI agent how to use the tool bundle to accomplish a common task. Every skill is CLI-first (it calls `devboy tools call <name>`), agent-agnostic (installable into Claude Code / Codex / Cursor / Kimi or a vendor-neutral path), and versioned with the binary.

```bash
devboy skills list                          # see the shipped catalogue
devboy skills install --all --agent all     # install every skill into every detected agent
devboy skills install devboy-review-mr      # repo-local by default
devboy skills upgrade                       # refresh every installed skill after `devboy upgrade`
```

| Category | Example skills |
|----------|---------------|
| `self-bootstrap` | `devboy-setup`, `devboy-repair`, `devboy-tools-catalog` |
| `issue-tracking` | `devboy-get-issues`, `devboy-create-issue`, `devboy-update-issue`, `devboy-link-issues`, `devboy-solve-issue` |
| `code-review` | `devboy-review-mr`, `devboy-fix-review-comments`, `devboy-self-review` |
| `self-feedback` | `devboy-run-and-verify`, `devboy-daily-report`, `devboy-retro`, `devboy-knowledge-extract`, `analyze-usage` |
| `meeting-notes` | `devboy-meeting-search`, `devboy-meeting-transcript`, `devboy-meeting-to-tasks` |
| `messenger` | `devboy-chat-search`, `devboy-chat-summary`, `devboy-notify` |

The design lives in `docs/architecture/adr/ADR-012-skills-subsystem.md`; the user guide is at `docs/guide/skills/`. Skill installs keep a per-location manifest with SHA256s so upgrades leave user-modified files alone (ADR-014), and the self-feedback category reads session traces written to `.devboy/sessions/` in the format defined by ADR-015.

### Featured skill: `analyze-usage` (split: thin baseline + fat backend)

`analyze-usage` is the first skill that ships in **two parts**:

1. **Thin baseline** at [`skills/03-self-feedback/analyze-usage/SKILL.md`](./skills/03-self-feedback/analyze-usage/SKILL.md) — installs through the standard catalogue (`devboy skills install analyze-usage`). Single markdown file, embedded in the binary. Tells the agent *what* to do.
2. **Fat backend** at [`./.claude/skills/analyze-usage/`](./.claude/skills/analyze-usage/) — Python pipeline (`bin/analyze-usage`, `lib/`, `scripts/`, parquet outputs). The agent fetches it on first use via curl-pipe-bash, no full clone:

   ```bash
   curl -sSL https://raw.githubusercontent.com/meteora-pro/devboy-tools/main/.claude/skills/analyze-usage/scripts/install.sh | bash
   ```

This pattern keeps the `devboy` binary small (no embedded Python code), lets the backend evolve independently of the binary release cadence, and still gives users a one-command install via the standard catalogue:

```bash
devboy skills install analyze-usage --agent claude   # baseline
# (the SKILL.md instructs the agent to curl-install the backend on first run)
```

Once installed, the skill auto-activates on triggers like *"weekly digest"*, *"DORA"*, *"когда сессия стала китом"*, *"drill into session 2c052d83"*. Or run the CLI directly:

```bash
~/.claude/skills/analyze-usage/bin/analyze-usage period \
    --from 2026-04-01 --to 2026-04-30 --format html --open
```

It produces a graphic monthly/weekly digest (terminal / markdown / html) with biome aquariums (🐋🦈🐬🐟🦐🦠), 8-archetype bars, rhythm, stack palette, DORA radar (CFR + lead time + pushes), friction markers; plus per-session parquet bundles (`outputs/raw/`, `outputs/anon/`, `outputs/llm/`) for further analysis.

- Backend readme: [`./.claude/skills/analyze-usage/README.md`](./.claude/skills/analyze-usage/README.md)
- Concept glossary (biome, archetype, rhythm, stack, DORA, friction, scaling laws): [`./.claude/skills/analyze-usage/GLOSSARY.md`](./.claude/skills/analyze-usage/GLOSSARY.md)
- Architecture reference (extractors, library API, anonymization contract): [`./.claude/skills/analyze-usage/SKILL.md`](./.claude/skills/analyze-usage/SKILL.md)
- Baseline skill (what `devboy skills install` ships): [`./skills/03-self-feedback/analyze-usage/SKILL.md`](./skills/03-self-feedback/analyze-usage/SKILL.md)

## Why DevBoy?

| | Others | DevBoy |
|-|--------|--------|
| **Privacy** | Cloud-based credentials | Local OS keychain + env vars for CI |
| **Focus** | All projects at once | Context-based project isolation |
| **Context** | Static tool descriptions | Dynamic per-project prompts |
| **Efficiency** | Default API responses | LLM-optimized pipeline — **~5–20% token savings** on real workloads (higher on large list/diff responses, lower on simple calls). Measured against our own production traffic and benchmarks — no cherry-picked numbers. |
| **Tools** | Generic aggregators | Purpose-built for dev workflows |
| **Extensibility** | Monolithic | Plugin system (Rust, WASM, TypeScript) |
| **Consumption** | MCP only | MCP **or** CLI **or** agent skills — same tool bundle |

## Architecture

### Contexts (Multi-Project)

One server supports multiple project contexts with instant switching:

```
┌─────────────────────────────────────┐
│           DevBoy MCP Server         │
├─────────────────────────────────────┤
│  Context: devboy-tools              │
│    └── GitHub: meteora-pro/devboy   │
│  Context: dashboard                 │
│    ├── GitLab: project #42          │
│    └── ClickUp: list abc123         │
└─────────────────────────────────────┘
```

Switch contexts via CLI (`devboy context use <name>`) or MCP tools (`use_context`).

### Crate Architecture

```
crates/
├── devboy-core/          # Traits (Provider, ToolEnricher), types, config
├── devboy-executor/      # Tool execution engine + enrichment pipeline
├── devboy-mcp/           # MCP server (JSON-RPC over stdio)
├── devboy-cli/           # CLI binary
├── devboy-storage/       # Credential storage (keychain, env vars)
└── plugins/
    ├── api/              # Provider integrations
    │   ├── gitlab/       # Client + GitLabSchemaEnricher
    │   ├── github/       # Client + GitHubSchemaEnricher
    │   ├── clickup/      # Client + ClickUpSchemaEnricher + metadata
    │   └── jira/         # Client + JiraSchemaEnricher + metadata
    └── pipeline/         # Output formatting (markdown, truncation)
```

### Executor & Enricher Pipeline

The `devboy-executor` crate separates tool execution from transport (MCP, HTTP, NAPI).
Each provider crate includes a schema enricher that dynamically adapts tool schemas:

```
Tool call → Executor
  1. Enrichers transform args (cf_story_points → customFields)
  2. Provider factory creates client from ProviderConfig
  3. Provider executes API calls → typed ToolOutput
  4. Pipeline formats output → text (markdown/compact/json)
```

Three enricher categories, same `ToolEnricher` trait:
- **Provider enrichers** — adapt schemas per provider (remove unsupported params, add custom field `cf_*` params, populate enums from metadata)
- **Pipeline enrichers** — add output control params (e.g., `format` enum)
- **Custom enrichers** — third-party plugins

### Plugin System

Tools are dynamic based on project configuration:

```
plugins/
├── api/           # Provider integrations (client + enricher per provider)
│   ├── gitlab/
│   ├── github/
│   ├── clickup/
│   └── jira/
└── pipeline/      # Data processing
    ├── pagination/
    ├── truncation/
    └── enrichment/
```

## Installation

### From npm (Recommended)

```bash
npm install -g @devboy-tools/cli
# or
pnpm add -g @devboy-tools/cli
```

The correct binary for your platform is installed automatically. Global install makes the `devboy` command available system-wide.

### From source

```bash
git clone https://github.com/meteora-pro/devboy-tools.git
cd devboy-tools
cargo build --release
```

### From releases

Download binary from [Releases](https://github.com/meteora-pro/devboy-tools/releases).

## Quick start

### 1. Configure Provider

```bash
# GitHub
./target/release/devboy config set github.owner <owner>
./target/release/devboy config set github.repo <repo>
./target/release/devboy config set-secret github.token <token>

# GitLab
./target/release/devboy config set gitlab.url https://gitlab.example.com
./target/release/devboy config set gitlab.project_id <project-id>
./target/release/devboy config set-secret gitlab.token <token>

# ClickUp
./target/release/devboy config set clickup.list_id <list-id>
./target/release/devboy config set clickup.team_id <team-id>  # recommended for custom task IDs
./target/release/devboy config set-secret clickup.token <token>
```

Tokens are stored securely in OS keychain (macOS Keychain, Windows Credential Manager, Linux Secret Service).

### Alternative: Environment Variables (CI/CD)

For CI/CD pipelines and containerized environments where keychain is unavailable, use environment variables:

```bash
# With DEVBOY_ prefix (recommended)
export DEVBOY_GITHUB_TOKEN=ghp_xxx
export DEVBOY_GITLAB_TOKEN=glpat-xxx
export DEVBOY_CLICKUP_TOKEN=pk_xxx
export DEVBOY_JIRA_TOKEN=xxx

# Or without prefix (compatible with other tools)
export GITHUB_TOKEN=ghp_xxx
export GITLAB_TOKEN=glpat-xxx
```

**Credential Resolution Order:**
1. Environment variables (`DEVBOY_{PROVIDER}_TOKEN`, then `{PROVIDER}_TOKEN`)
2. OS Keychain

This allows seamless use in GitHub Actions, GitLab CI, Docker, and cloud workspaces.

### 2. Verify Connection

```bash
./target/release/devboy test github
```

### 3. Test MCP Server

```bash
./scripts/test-mcp.sh
```

## Integration with AI Assistants

### Claude Code (CLI)

```bash
claude mcp add devboy -- /path/to/devboy-tools/target/release/devboy mcp
```

Verify:
```bash
claude mcp list
```

### Claude Desktop

Add to `~/Library/Application Support/Claude/claude_desktop_config.json`:

```json
{
  "mcpServers": {
    "devboy": {
      "command": "/path/to/devboy-tools/target/release/devboy",
      "args": ["mcp"]
    }
  }
}
```

## CLI Commands

```bash
devboy --help                           # Show all commands
devboy config list                      # Show current configuration
devboy config path                      # Show config file location
devboy config set <key> <value>         # Set config value
devboy config set-secret <key> <value>  # Store secret in keychain
devboy config get <key>                 # Get config value
devboy context list                     # List contexts, show active
devboy context use <name>               # Switch active context
devboy issues                           # List issues
devboy mrs                              # List merge requests
devboy test <provider>                  # Test provider connection
devboy doctor                           # Run all diagnostic checks
devboy doctor --list-checks             # List available doctor check IDs
devboy doctor --checks <checks...>      # Run only selected checks
devboy doctor --format json             # Emit JSON output
devboy mcp                              # Start MCP server (stdio)
devboy tools                            # Interactive tool management (TUI)
devboy tools list                       # List tools with enabled/disabled status
devboy tools disable <names...>         # Disable specific built-in tools
devboy tools enable <names...>          # Re-enable specific tools
devboy tools reset                      # Reset all filtering
devboy tools call <name> [args]         # Call a built-in tool directly
devboy tools docs --output FILE         # Auto-generate the tool reference (Markdown / JSON)
devboy docs cli   --output FILE         # Auto-generate this CLI reference from the live `clap` definition
```

The full, always-up-to-date listing lives in
[`docs/guide/reference/cli.md`](docs/guide/reference/cli.md) — refreshed
automatically by `devboy docs cli` and gated in CI.

### Doctor command

```bash
# Run the full diagnostic suite
devboy doctor

# List all available checks
devboy doctor --list-checks

# Run a subset of checks (comma-delimited or repeated --checks flags)
devboy doctor --checks config.exists,config.valid_toml

# Machine-readable output for CI or scripts
devboy doctor --format json
devboy doctor --format json --checks providers.github
```

## Development

```bash
# Build
cargo build

# Run tests
cargo test

# Lint
cargo clippy

# Build release
cargo build --release
```

See [CONTRIBUTING.md](CONTRIBUTING.md) for guidelines.

## Related projects

- [devboy-tools-agent-usage](https://github.com/meteora-pro/devboy-tools-agent-usage) — CLI tool for analyzing AI agent usage (Claude Code): cost, time, tasks, focus. Reads JSONL logs and provides token/cost breakdowns, task grouping by git branch, tool call categories, and session timeline visualization.

## Coverage report

[![Codecov](https://codecov.io/gh/meteora-pro/devboy-tools/branch/main/graph/badge.svg)](https://codecov.io/gh/meteora-pro/devboy-tools)

Detailed coverage reports are available on [Codecov](https://codecov.io/gh/meteora-pro/devboy-tools).

## License

[Apache License 2.0](LICENSE)
