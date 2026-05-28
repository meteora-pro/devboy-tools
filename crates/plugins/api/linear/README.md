# devboy-linear

[![Crates.io](https://img.shields.io/crates/v/devboy-linear.svg)](https://crates.io/crates/devboy-linear)
[![Docs.rs](https://docs.rs/devboy-linear/badge.svg)](https://docs.rs/devboy-linear)
[![License](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](https://opensource.org/licenses/Apache-2.0)

Linear provider for [`devboy-tools`](https://github.com/meteora-pro/devboy-tools).

This crate provides a GraphQL-backed Linear issue tracker provider for `devboy-tools`.

Current capabilities:
- authenticated user lookup
- liveness probing
- issue listing and single-issue lookup
- issue creation and updates
- issue comment listing and comment creation

The schema enricher includes Linear-specific issue schema adjustments such as
priority enums and supported filter parameters. Team-specific workflow state
enumeration is not yet metadata-driven.

## Add to your project

```toml
[dependencies]
devboy-core = "0.30"
devboy-linear = "0.30"
```

## License

Apache-2.0 — see [LICENSE](https://github.com/meteora-pro/devboy-tools/blob/main/LICENSE).
