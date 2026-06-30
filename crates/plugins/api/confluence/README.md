# devboy-confluence

[![Crates.io](https://img.shields.io/crates/v/devboy-confluence.svg)](https://crates.io/crates/devboy-confluence)
[![Docs.rs](https://docs.rs/devboy-confluence/badge.svg)](https://docs.rs/devboy-confluence)
[![License](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](https://opensource.org/licenses/Apache-2.0)

Confluence provider for [`devboy-tools`](https://github.com/meteora-pro/devboy-tools). Implements `KnowledgeBaseProvider` and `Provider` from [`devboy-core`](https://crates.io/crates/devboy-core) for Atlassian Confluence across both Confluence Cloud and Confluence Server / Data Center.

## Features

- Shared `ConfluenceFlavor::{Cloud, SelfHosted}` architecture
- Knowledge base page and space operations
- Confluence Cloud routing via Atlassian `cloud_id`
- Atlassian OAuth helpers for Cloud bearer auth
- Basic auth support for Cloud and self-hosted deployments
- ADF-to-Markdown conversion for Cloud page content
- Liveness probing for both deployment flavors

## Add to your project

```toml
[dependencies]
devboy-core = "0.26"
devboy-confluence = "0.26"
```

## Documentation

API reference on [docs.rs/devboy-confluence](https://docs.rs/devboy-confluence).

For configuration, auth flows, and CLI usage, see the Confluence integration guide in the main repository docs.

For the full bundle (CLI, MCP server, agent skills) see the [main project README](https://github.com/meteora-pro/devboy-tools#readme).

## License

Apache-2.0 — see [LICENSE](https://github.com/meteora-pro/devboy-tools/blob/main/LICENSE).
