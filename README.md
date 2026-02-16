# DevBoy Tools

[![CI](https://github.com/meteora-pro/devboy-tools/actions/workflows/ci.yml/badge.svg)](https://github.com/meteora-pro/devboy-tools/actions/workflows/ci.yml)
[![Codecov](https://codecov.io/gh/meteora-pro/devboy-tools/branch/main/graph/badge.svg)](https://codecov.io/gh/meteora-pro/devboy-tools)
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

### From Source (Recommended for Development)

```bash
git clone https://github.com/meteora-pro/devboy-tools.git
cd devboy-tools
cargo build --release
```

### From Releases

Download binary from [Releases](https://github.com/meteora-pro/devboy-tools/releases).

## Quick Start

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
./target/release/devboy config set clickup.team_id <team-id>
./target/release/devboy config set clickup.list_id <list-id>
./target/release/devboy config set-secret clickup.token <token>
```

Tokens are stored securely in OS keychain (macOS Keychain, Windows Credential Manager, Linux Secret Service).

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
devboy --help                     # Show all commands
devboy config list                # Show current configuration
devboy config path                # Show config file location
devboy config set <key> <value>   # Set config value
devboy config set-secret <key> <value>  # Store secret in keychain
devboy config get <key>           # Get config value
devboy issues                     # List issues
devboy mrs                        # List merge requests
devboy test <provider>            # Test provider connection
devboy mcp                        # Start MCP server (stdio)
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

## Coverage Report

[![Codecov](https://codecov.io/gh/meteora-pro/devboy-tools/branch/main/graph/badge.svg)](https://codecov.io/gh/meteora-pro/devboy-tools)

Detailed coverage reports are available on [Codecov](https://codecov.io/gh/meteora-pro/devboy-tools).

## License

[Apache License 2.0](LICENSE)
