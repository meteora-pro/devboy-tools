# DevBoy

[![CI](https://github.com/meteora-pro/devboy-tools/actions/workflows/ci.yml/badge.svg)](https://github.com/meteora-pro/devboy-tools/actions/workflows/ci.yml)
[![Codecov](https://codecov.io/gh/meteora-pro/devboy-tools/branch/main/graph/badge.svg)](https://codecov.io/gh/meteora-pro/devboy-tools)
[![License](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](https://opensource.org/licenses/Apache-2.0)
[![npm](https://img.shields.io/npm/v/@devboy-tools/cli)](https://www.npmjs.com/package/@devboy-tools/cli)
[![Ask Zread](https://img.shields.io/badge/Ask_Zread-_.svg?style=flat&color=00b0aa&labelColor=000000)](https://zread.ai/meteora-pro/devboy-tools)

**A research-driven dev-workflow toolkit for AI coding agents.** Connects your agent (Claude Code, Copilot CLI, Codex, Cursor, Kimi, Gemini, …) to GitHub, GitLab, Jira, ClickUp, Slack, and Fireflies — through MCP, CLI, or installable agent skills. Output goes through a token-aware pipeline that, on real production traffic, cuts response size by 5–20%.

```bash
npm install -g @devboy-tools/cli   # binary for your platform
devboy onboard                     # detects your AI agent, installs the right skills
```

That's it. Verify with `devboy doctor`.

---

## Why DevBoy

DevBoy isn't another aggregator with a long tool list. Four things that aren't standard elsewhere:

- **Research-driven.** Every optimisation in the pipeline traces back to a paper grounded in a real corpus — 523 Claude Code sessions, 10,644 MCP tool responses. We don't ship a heuristic without measuring it. See [research index](docs/research/INDEX.md).
- **Three transports, one bundle.** The exact same tool set is reachable as MCP server, CLI for humans / CI, or as agent skills that call individual tools. Pick whichever fits today; layer the rest later.
- **Privacy by default.** Tokens live in OS keychain (macOS Keychain / Windows Credential Manager / Linux Secret Service) with env-var fallback for CI. No cloud round-trip just to authenticate.
- **Multi-project context.** One server, many project contexts (different GitHub/GitLab/Jira combinations), instant switch — no respawn, no config edit. Concrete: `devboy context use dashboard` and the same MCP session now talks to a different project's APIs.

| | Generic MCP aggregator | DevBoy |
|-|------------------------|--------|
| **Output efficiency** | Default API JSON | Knapsack-based pagination + format-adaptive encoding (papers 1, 2) — typically 5–20% token savings |
| **Tool catalogue** | Static | Dynamic per-project, with provider enrichers (custom field params, enum hints) |
| **Transport** | MCP only | MCP, CLI, or agent skills — same tools |
| **Onboarding** | Manual config + per-agent install | `devboy onboard` autodetects and bundles |
| **Credentials** | Cloud / config files | OS keychain, env vars only as fallback |
| **Extensibility** | Forking | Plugin system (Rust today; WASM, TypeScript planned) |

---

## Quick start (60 seconds)

```bash
# 1. Install
npm install -g @devboy-tools/cli

# 2. Bootstrap — picks your agent + installs a curated skill bundle
devboy onboard

# 3. Configure your first provider (interactive)
devboy init

# 4. Verify
devboy doctor
```

After this `devboy issues` returns your open tickets, your agent has the relevant skills loaded, and the MCP server is registered with whatever client you use.

If you'd rather pick everything by hand:

<details>
<summary><b>Manual install / configuration</b></summary>

```bash
# Configure GitHub (replace gitlab/clickup/jira similarly)
devboy config set github.owner meteora-pro
devboy config set github.repo devboy-tools
devboy config set-secret github.token <token>     # → OS keychain

# Or via env vars (CI / Docker — keychain unavailable)
export DEVBOY_GITHUB_TOKEN=ghp_...
# Compatibility: GITHUB_TOKEN is read too

# Pick skills explicitly instead of using a profile
devboy skills list
devboy skills install devboy-review-mr --agent claude
devboy skills install --all --agent all
```

Build from source:
```bash
git clone https://github.com/meteora-pro/devboy-tools.git
cd devboy-tools && cargo build --release
./target/release/devboy --version
```
</details>

---

## Skills & onboarding

DevBoy ships a catalogue of **skills** — one-page Markdown recipes that tell an AI agent how to use the bundle to accomplish a common task. Skills are CLI-first (`devboy tools call <name>` under the hood), agent-agnostic (Claude Code / Codex / Cursor / Kimi or a vendor-neutral path), and versioned with the binary.

`devboy onboard` is the fastest path: it scans `~/.claude/`, `~/.copilot/`, `~/.codex/`, `~/.kimi/`, Cursor's storage, `~/.gemini/`, and `~/.gemini/antigravity/`, scores each agent on freshness × volume (recency wins ties), and installs a profile-specific bundle.

```bash
devboy onboard                          # auto-detect + install `dev` bundle
devboy onboard --profile pm             # PM bundle (issues + meetings + chat)
devboy onboard --profile oncall         # diagnostics + notifications
devboy onboard --agent kimi --yes       # explicit agent + non-interactive
devboy agents list                      # show all detected agents with score
```

Three profiles ship today; categories below cover the full catalogue.

| Category | Skills |
|----------|--------|
| `self-bootstrap` | `devboy-setup`, `devboy-repair`, `devboy-tools-catalog`, `devboy-pipeline-tune` |
| `issue-tracking` | `devboy-get-issues`, `devboy-create-issue`, `devboy-update-issue`, `devboy-link-issues`, `devboy-solve-issue` |
| `code-review` | `devboy-review-mr`, `devboy-fix-review-comments`, `devboy-self-review` |
| `self-feedback` | `devboy-run-and-verify`, `devboy-daily-report`, `devboy-retro`, `devboy-knowledge-extract`, `devboy-qa-sweep`, `analyze-usage` |
| `meeting-notes` | `devboy-meeting-search`, `devboy-meeting-transcript`, `devboy-meeting-to-tasks` |
| `messenger` | `devboy-chat-search`, `devboy-chat-summary`, `devboy-notify` |

Skill installs keep a per-location manifest with SHA-256s so upgrades leave user-modified files alone ([ADR-014](docs/architecture/adr/ADR-014-skills-lifecycle.md)). Self-feedback skills read session traces from `.devboy/sessions/` ([ADR-015](docs/architecture/adr/ADR-015-skills-session-traces.md)).

**`analyze-usage`** is a featured skill that ships in two parts: a thin baseline (one Markdown file, embedded in the binary) plus a heavier Python backend (`~1 MB`, sparse-checked-out via curl on first use). It produces graphic monthly / weekly digests of how your AI sessions actually went — biome aquariums, 8-archetype bars, DORA radar, friction markers — plus shareable anonymised parquet bundles. See [`./.claude/skills/analyze-usage/`](./.claude/skills/analyze-usage/).

---

## Three integration modes

The same tool set, three transports — pick what your workflow already uses.

| Mode | When to use | Example |
|------|-------------|---------|
| **MCP server** | Claude Desktop, Claude Code, any MCP-compatible client | `devboy mcp` (stdio) |
| **CLI** | Humans at the terminal, CI jobs, shell scripts | `devboy issues`, `devboy mrs`, `devboy tools call get_issues '{"limit": 20}'` |
| **Agent skills** | Agents that don't want the full MCP tool-list tax — call only the tools a skill needs | `devboy tools call get_issues` from inside a skill script |

> **JSON arguments tip.** `devboy tools call <name>` takes an optional positional JSON string (defaults to `{}`). POSIX shells: wrap in single quotes. Windows `cmd.exe`/PowerShell: escape inner quotes — `devboy tools call get_issues "{\"limit\": 20}"`.

### Claude Code

```bash
claude mcp add devboy -- devboy mcp
claude mcp list
```

### Claude Desktop

`~/Library/Application Support/Claude/claude_desktop_config.json`:

```json
{ "mcpServers": { "devboy": { "command": "devboy", "args": ["mcp"] } } }
```

For Codex / Cursor / Kimi / Copilot CLI / Gemini CLI / Antigravity — `devboy onboard` autoconfigures the MCP entry; or follow the agent's docs for adding a stdio MCP server pointing at `devboy mcp`.

---

## Providers

Six provider plugins ship today — each with a dedicated client + schema enricher so the tool list adapts to your project's actual fields (custom fields, enum values, status taxonomies):

| Provider | Crate | What you get |
|----------|-------|--------------|
| **GitHub** | [`devboy-github`](crates/plugins/api/github/) | Issues, pull requests, comments, branches, repos |
| **GitLab** | [`devboy-gitlab`](crates/plugins/api/gitlab/) | Issues, merge requests, discussions, pipelines, MR diffs |
| **Jira** | [`devboy-jira`](crates/plugins/api/jira/) | Issues with custom-field metadata, sprints, transitions |
| **ClickUp** | [`devboy-clickup`](crates/plugins/api/clickup/) | Tasks, custom fields, lists, custom task IDs |
| **Slack** | [`devboy-slack`](crates/plugins/api/slack/) | Chat search, channel summary, post message |
| **Fireflies** | [`devboy-fireflies`](crates/plugins/api/fireflies/) | Meeting transcripts, search, action items |

Adding a provider is a Rust crate implementing `Provider` + a `ToolEnricher` ([ADR-007](docs/architecture/adr/ADR-007-plugin-architecture.md)).

---

## Research

Every non-trivial optimisation in the pipeline is backed by a paper grounded in a real corpus — 523 Claude Code sessions, 10,644 MCP responses from production traffic. The full [`docs/research/INDEX.md`](docs/research/INDEX.md) tracks methods, datasets, and reproducibility scripts.

| # | Title | Status | Core idea |
|---|-------|--------|-----------|
| [1](docs/research/paper-1-trimtree.md) | TrimTree: priority-driven pagination | draft | Binary knapsack for which items to include within a token budget; **p₁** metric |
| [2](docs/research/paper-2-mckp-format-adaptive.md) | Format-adaptive tree encoding | draft | Multi-choice knapsack: per-subtree format pick (CSV / table / key:value) to minimise tokens |
| [3](docs/research/paper-3-tool-aware-enrichment.md) | Tool-aware context enrichment | draft, **deployed** | Thin initial context inflates follow-up tool calls; measurement + mitigation |
| [4](docs/research/paper-4-notebook-parquet.md) | Dataset-as-context (notebook + Parquet) | draft | Large responses become queryable Parquet artefacts; LLM writes pull-queries instead of paginating |

Highlights from the corpus:

- `get_merge_request_diffs`: P90 = 35 k chars ≈ 10 k tokens — **28%** of responses exceed an 8 k budget
- `get_epics`: P90 = 43 k chars ≈ 12 k tokens — 37% exceed budget
- After a large response: agents always continue with text on the next turn — they **never** paginate
- Thin issues (< 200 chars/item) → 43% of turns add a follow-up `get_issue` call. Rich issues (1.5 k–4 k chars) → 2%. Pearson r = −0.28

These findings drive concrete code: paper 3 is already shipping in the format pipeline; papers 1 and 2 are landing in the next minor version.

---

## Architecture

<details>
<summary><b>Crate layout</b></summary>

```
crates/
├── devboy-core/        Traits (Provider, ToolEnricher), shared types, config
├── devboy-executor/    Tool execution engine + enrichment pipeline
├── devboy-mcp/         MCP server (JSON-RPC over stdio)
├── devboy-cli/         CLI binary (`devboy`)
├── devboy-skills/      Skill catalogue, install/upgrade, manifests, traces
├── devboy-storage/     Credential storage (keychain, env vars)
├── devboy-assets/      File attachments (ADR-010)
└── plugins/
    ├── api/            { github, gitlab, jira, clickup, slack, fireflies }
    └── format-pipeline The token-aware output pipeline (papers 1, 2, 3)
```

</details>

<details>
<summary><b>Multi-project contexts</b></summary>

One server, many contexts. Each context is its own provider config bundle:

```
┌─ DevBoy MCP / CLI ────────────────┐
│  context: devboy-tools             │
│    ├── GitHub: meteora-pro/devboy  │
│    └── Slack: #devboy              │
│  context: dashboard                │
│    ├── GitLab: project #42         │
│    ├── ClickUp: list abc123        │
│    └── Jira: DEV                   │
└────────────────────────────────────┘
```

Switch with `devboy context use <name>` (CLI) or the `use_context` tool (MCP). No respawn — the active session re-reads the new bindings on the next call.

</details>

<details>
<summary><b>Executor + enricher pipeline</b></summary>

```
Tool call → Executor
  1. Enrichers transform args   (e.g. cf_story_points → customFields)
  2. Provider factory builds the client from ProviderConfig
  3. Provider executes API calls → typed ToolOutput
  4. Format pipeline encodes output → text (markdown / compact / json)
```

Three enricher categories, single `ToolEnricher` trait:

- **Provider enrichers** — adapt schemas per provider (drop unsupported params, surface custom-field params, populate enums from project metadata).
- **Pipeline enrichers** — add output-control parameters (`format` enum, pagination knobs).
- **Custom enrichers** — third-party plugins.

Architecture details: [executor](docs/guide/architecture/executor.md), [enrichers](docs/guide/architecture/enrichers.md), [format pipeline](docs/guide/architecture/format-pipeline.md).

</details>

---

## Documentation map

- **Getting started** — [`docs/guide/getting-started/`](docs/guide/getting-started/)
- **CLI reference** (auto-generated) — [`docs/guide/reference/cli.md`](docs/guide/reference/cli.md)
- **Tool reference** (auto-generated) — [`docs/guide/reference/tools.md`](docs/guide/reference/tools.md)
- **Skills user guide** — [`docs/guide/skills/`](docs/guide/skills/)
- **Configuration** (env vars, contexts, doctor, proxy, format pipeline) — [`docs/guide/configuration/`](docs/guide/configuration/)
- **Architecture** — [`docs/guide/architecture/`](docs/guide/architecture/)
- **ADRs** — [`docs/architecture/adr/INDEX.md`](docs/architecture/adr/INDEX.md) (17 decisions logged)
- **Research papers** — [`docs/research/INDEX.md`](docs/research/INDEX.md)

---

## Development

```bash
cargo build                        # debug build
cargo test                         # runs the workspace test suite
cargo clippy --all-targets         # lint (CI uses RUSTFLAGS=-Dwarnings)
cargo fmt --all                    # format
cargo run -p devboy-cli -- doctor  # smoke
```

The CLI reference is gated in CI: after touching `clap` definitions, run

```bash
cargo run -p devboy-cli -- docs cli --output docs/guide/reference/cli.md
```

so the committed reference matches the binary. Same idea for `devboy tools docs` and the tool reference.

See [CONTRIBUTING.md](CONTRIBUTING.md) for the full guide (commit conventions, branch naming, ADR workflow, release process).

---

## Community

- **Issues / feature requests** — [GitHub Issues](https://github.com/meteora-pro/devboy-tools/issues)
- **Design discussions** — [GitHub Discussions](https://github.com/meteora-pro/devboy-tools/discussions)
- **Code review tooling** — open a PR; CI runs `Format`, `Clippy`, `Test` on macOS / Linux / Windows, `Coverage`, and the docs drift gate

## License

[Apache License 2.0](LICENSE) — use it, modify it, ship it; if you build something interesting on top, we'd love a heads-up via Discussions.
