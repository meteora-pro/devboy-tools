# @devboy-tools/cli

[![npm](https://img.shields.io/npm/v/@devboy-tools/cli)](https://www.npmjs.com/package/@devboy-tools/cli)
[![License](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](https://opensource.org/licenses/Apache-2.0)

npm distribution of [DevBoy tools](https://github.com/meteora-pro/devboy-tools) — a fast, configurable **tool bundle** for AI coding agents, written in Rust. Use the same tools via an MCP server, via CLI commands, or directly from agent skills.

The correct binary for your platform is installed automatically via platform-specific packages.

## Supported platforms

| OS      | Architecture | Package                      |
|---------|-------------|------------------------------|
| macOS   | ARM64       | `@devboy-tools/darwin-arm64` |
| macOS   | x64         | `@devboy-tools/darwin-x64`   |
| Linux   | x64         | `@devboy-tools/linux-x64`    |
| Linux   | ARM64       | `@devboy-tools/linux-arm64`  |
| Windows | x64         | `@devboy-tools/win32-x64`    |

## Installation

```bash
npm install @devboy-tools/cli
# or
pnpm add @devboy-tools/cli
```

## Usage

DevBoy is a tool bundle — pick the mode that fits your workflow:

### CLI (humans, CI, shell scripts)

```bash
# Show help
npx devboy --help

# Configure a provider
npx devboy config set github.owner <owner>
npx devboy config set github.repo <repo>
npx devboy config set-secret github.token <token>

# Use tools directly
npx devboy issues
npx devboy mrs
```

### MCP server (Claude Desktop, Claude Code, any MCP client)

```bash
# Start MCP server over stdio
npx devboy mcp
```

### Agent skills (call a single tool from a skill script)

```bash
# Invoke a specific tool without paying the MCP tool-list tax
npx devboy tools call get_issues '{"limit": 20}'
```

### Claude Code

```bash
claude mcp add devboy -- npx devboy mcp
```

### Claude Desktop

Add to `~/Library/Application Support/Claude/claude_desktop_config.json`:

```json
{
  "mcpServers": {
    "devboy": {
      "command": "npx",
      "args": ["devboy", "mcp"]
    }
  }
}
```

### Programmatic API

```javascript
const { getBinaryPath, name, version } = require("@devboy-tools/cli");

console.log(getBinaryPath()); // /path/to/node_modules/@devboy-tools/darwin-arm64/bin/devboy
console.log(name);            // "devboy"
console.log(version);         // "0.3.0"
```

```typescript
import { getBinaryPath, name, version } from "@devboy-tools/cli";
```

## Environment variables

| Variable             | Description                                      |
|----------------------|--------------------------------------------------|
| `DEVBOY_BINARY_PATH` | Override binary path (skips platform package resolution) |

## Troubleshooting

### Binary not found after install

```bash
npm rebuild @devboy-tools/cli
```

### Unsupported platform

If your platform is not listed above, build from source:

```bash
cargo install --git https://github.com/meteora-pro/devboy-tools.git
export DEVBOY_BINARY_PATH=$(which devboy)
```

## License

[Apache License 2.0](https://github.com/meteora-pro/devboy-tools/blob/main/LICENSE)
