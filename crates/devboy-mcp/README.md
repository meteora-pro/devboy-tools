# devboy-mcp

[![Crates.io](https://img.shields.io/crates/v/devboy-mcp.svg)](https://crates.io/crates/devboy-mcp)
[![Docs.rs](https://docs.rs/devboy-mcp/badge.svg)](https://docs.rs/devboy-mcp)
[![License](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](https://opensource.org/licenses/Apache-2.0)

MCP (Model Context Protocol) server for [`devboy-tools`](https://github.com/meteora-pro/devboy-tools). Exposes every devboy provider as MCP tools to AI agents — Claude Code, Codex, Cursor, Copilot CLI — over JSON-RPC 2.0 on stdio.

## Add to your project

```toml
[dependencies]
devboy-mcp = "0.26"
```

You also need:

- [`devboy-core`](https://crates.io/crates/devboy-core) for the trait surface
- [`devboy-executor`](https://crates.io/crates/devboy-executor) to drive concrete providers
- One or more provider crates (e.g. [`devboy-gitlab`](https://crates.io/crates/devboy-gitlab), [`devboy-github`](https://crates.io/crates/devboy-github))

## Documentation

API reference on [docs.rs/devboy-mcp](https://docs.rs/devboy-mcp).

For the full bundle (CLI, agent skills, format pipeline) see the [main project README](https://github.com/meteora-pro/devboy-tools#readme).

## License

Apache-2.0 — see [LICENSE](https://github.com/meteora-pro/devboy-tools/blob/main/LICENSE).
