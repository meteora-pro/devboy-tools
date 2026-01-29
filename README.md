# DevBoy Tools

[![CI](https://github.com/meteora-pro/devboy-tools/actions/workflows/ci.yml/badge.svg)](https://github.com/meteora-pro/devboy-tools/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](https://opensource.org/licenses/Apache-2.0)

Fast and efficient Open Source MCP server written in Rust. Designed for coding agents with plugin system (API providers + LLM-optimized pipeline) and project-scoped isolation (1 server = 1 project).

## Why DevBoy?

| | Others | DevBoy |
|-|--------|--------|
| **Privacy** | Cloud-based credentials | Local OS keychain |
| **Focus** | All projects at once | 1 server = 1 project (intentional) |
| **Context** | Static tool descriptions | Dynamic per-project prompts |
| **Efficiency** | Raw JSON (~2000 tokens) | Optimized output (~100 tokens) |
| **Tools** | Generic aggregators | Purpose-built for dev workflows |
| **Extensibility** | Monolithic | Plugin system (Rust, WASM, TypeScript) |

## Architecture

### One Server = One Project

Intentional constraint for focused AI context:

```
┌─────────────────────────────────────┐
│           DevBoy MCP Server         │
├─────────────────────────────────────┤
│  1 Repository                       │
│  1 Task List                        │
│  1 Set of Configured Plugins        │
└─────────────────────────────────────┘
```

For multi-project workflows → run multiple DevBoy servers.

### Plugin System

Tools are dynamic based on project configuration:

```
plugins/
├── api/           # Provider integrations
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

```bash
npx @devboy/tools serve
```

Or download binary from [Releases](https://github.com/meteora-pro/devboy-tools/releases).

## Quick Start

```bash
# Configure provider (token saved to OS keychain)
devboy config gitlab \
  --url https://gitlab.example.com \
  --project my/project \
  --token glpat-xxxxx

# Start MCP server
devboy serve
```

### Claude Desktop

```json
{
  "mcpServers": {
    "devboy": {
      "command": "npx",
      "args": ["@devboy/tools", "serve"]
    }
  }
}
```

## Development

```bash
cargo build && cargo test && cargo clippy
```

See [CONTRIBUTING.md](CONTRIBUTING.md) for guidelines.

## License

[Apache License 2.0](LICENSE)
