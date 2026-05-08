# devboy-assets

[![Crates.io](https://img.shields.io/crates/v/devboy-assets.svg)](https://crates.io/crates/devboy-assets)
[![Docs.rs](https://docs.rs/devboy-assets/badge.svg)](https://docs.rs/devboy-assets)
[![License](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](https://opensource.org/licenses/Apache-2.0)

Asset management for [`devboy-tools`](https://github.com/meteora-pro/devboy-tools) — on-disk cache, LRU rotation, and an atomic-write index for AI-agent tool outputs (large diffs, downloaded artifacts, transcripts).

Implements [ADR-010 — asset management](https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-010-asset-management.md).

## Add to your project

```toml
[dependencies]
devboy-assets = "0.26"
```

## Documentation

API reference on [docs.rs/devboy-assets](https://docs.rs/devboy-assets).

For the full bundle see the [main project README](https://github.com/meteora-pro/devboy-tools#readme).

## License

Apache-2.0 — see [LICENSE](https://github.com/meteora-pro/devboy-tools/blob/main/LICENSE).
