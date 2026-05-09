# devboy-core

[![Crates.io](https://img.shields.io/crates/v/devboy-core.svg)](https://crates.io/crates/devboy-core)
[![Docs.rs](https://docs.rs/devboy-core/badge.svg)](https://docs.rs/devboy-core)
[![License](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](https://opensource.org/licenses/Apache-2.0)

Core traits, types, and error handling for [`devboy-tools`](https://github.com/meteora-pro/devboy-tools) — the AI-coding-agent tool bundle.

## What this crate gives you

- **Provider traits.** `Provider`, `IssueProvider`, `MergeRequestProvider`, `KnowledgeBaseProvider`, `MessengerProvider` — the contract every devboy provider plugin (`devboy-gitlab`, `devboy-github`, `devboy-jira`, …) implements.
- **Unified types.** `Issue`, `MergeRequest`, `KbSpace`, `KbPage`, `MessengerChat`, `MessengerMessage`, `Discussion`, `Comment`, `FileDiff` — provider-agnostic representations of common collaboration objects.
- **Configuration.** `Config`, `GitHubConfig`, `GitLabConfig`, etc. — a TOML-driven multi-context configuration model.
- **Error handling.** `Error` / `Result` carrying contextual details, plus optional Sentry integration behind the `sentry` feature.

## Add to your project

```toml
[dependencies]
devboy-core = "0.26"
```

If you want a concrete provider, also add the matching plugin crate (e.g. `devboy-gitlab`, `devboy-github`).

## Documentation

API reference on [docs.rs/devboy-core](https://docs.rs/devboy-core).

For the full bundle (CLI, MCP server, agent skills, format pipeline) see the [main project README](https://github.com/meteora-pro/devboy-tools#readme).

## License

Apache-2.0 — see [LICENSE](https://github.com/meteora-pro/devboy-tools/blob/main/LICENSE).
