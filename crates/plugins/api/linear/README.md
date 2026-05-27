# devboy-linear

[![Crates.io](https://img.shields.io/crates/v/devboy-linear.svg)](https://crates.io/crates/devboy-linear)
[![Docs.rs](https://docs.rs/devboy-linear/badge.svg)](https://docs.rs/devboy-linear)
[![License](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](https://opensource.org/licenses/Apache-2.0)

Linear provider for [`devboy-tools`](https://github.com/meteora-pro/devboy-tools).

This crate currently provides the provider skeleton, authenticated user lookup, and liveness probing against Linear's GraphQL API. Issue operations land in follow-up changes.

## Add to your project

```toml
[dependencies]
devboy-core = "0.30"
devboy-linear = "0.30"
```

## License

Apache-2.0 — see [LICENSE](https://github.com/meteora-pro/devboy-tools/blob/main/LICENSE).
