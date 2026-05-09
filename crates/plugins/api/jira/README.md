# devboy-jira

[![Crates.io](https://img.shields.io/crates/v/devboy-jira.svg)](https://crates.io/crates/devboy-jira)
[![Docs.rs](https://docs.rs/devboy-jira/badge.svg)](https://docs.rs/devboy-jira)
[![License](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](https://opensource.org/licenses/Apache-2.0)

Jira provider for [`devboy-tools`](https://github.com/meteora-pro/devboy-tools). Implements `IssueProvider` and `Provider` from [`devboy-core`](https://crates.io/crates/devboy-core) against the Jira Cloud REST API, including project version CRUD.

## Add to your project

```toml
[dependencies]
devboy-core = "0.26"
devboy-jira = "0.26"
```

## Documentation

API reference on [docs.rs/devboy-jira](https://docs.rs/devboy-jira).

For the full bundle (CLI, MCP server, agent skills) see the [main project README](https://github.com/meteora-pro/devboy-tools#readme).

## License

Apache-2.0 — see [LICENSE](https://github.com/meteora-pro/devboy-tools/blob/main/LICENSE).
