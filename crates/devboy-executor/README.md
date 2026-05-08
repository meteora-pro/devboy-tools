# devboy-executor

[![Crates.io](https://img.shields.io/crates/v/devboy-executor.svg)](https://crates.io/crates/devboy-executor)
[![Docs.rs](https://docs.rs/devboy-executor/badge.svg)](https://docs.rs/devboy-executor)
[![License](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](https://opensource.org/licenses/Apache-2.0)

Tool-execution engine for [`devboy-tools`](https://github.com/meteora-pro/devboy-tools). Wires every devboy provider (`devboy-gitlab`, `devboy-github`, `devboy-jira`, `devboy-clickup`, `devboy-confluence`, `devboy-fireflies`, `devboy-slack`) into a uniform tool surface, applies the format-pipeline enrichment stage, and returns typed output suitable for MCP or CLI transport.

## Add to your project

```toml
[dependencies]
devboy-executor = "0.26"
```

## Documentation

API reference on [docs.rs/devboy-executor](https://docs.rs/devboy-executor).

See [ADR-007 — plugin architecture](https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-007-plugin-architecture.md) for the enricher model.

For the full bundle see the [main project README](https://github.com/meteora-pro/devboy-tools#readme).

## License

Apache-2.0 — see [LICENSE](https://github.com/meteora-pro/devboy-tools/blob/main/LICENSE).
