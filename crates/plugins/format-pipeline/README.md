# devboy-format-pipeline

[![Crates.io](https://img.shields.io/crates/v/devboy-format-pipeline.svg)](https://crates.io/crates/devboy-format-pipeline)
[![Docs.rs](https://docs.rs/devboy-format-pipeline/badge.svg)](https://docs.rs/devboy-format-pipeline)
[![License](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](https://opensource.org/licenses/Apache-2.0)

Format pipeline for [`devboy-tools`](https://github.com/meteora-pro/devboy-tools) — TOON encoding, MCKP-based tree-budget trimming, cursor pagination, deduplication. The output stage shared by every devboy provider.

Research-driven: every transformation traces back to a paper grounded in the project's 144k-event production corpus. Measured per-call savings on the data-shape-friendly endpoints (issues, pipelines, large lists): **26–69% on average, 92% top per call**.

- [Paper 1 — TOON encoding](https://github.com/meteora-pro/devboy-tools/blob/main/docs/research/paper-1-toon-encoding.md)
- [Paper 2 — MCKP format-adaptive pagination](https://github.com/meteora-pro/devboy-tools/blob/main/docs/research/paper-2-mckp-format-adaptive.md)

## Add to your project

```toml
[dependencies]
devboy-format-pipeline = "0.26"
```

## Documentation

API reference on [docs.rs/devboy-format-pipeline](https://docs.rs/devboy-format-pipeline).

For the full bundle see the [main project README](https://github.com/meteora-pro/devboy-tools#readme).

## License

Apache-2.0 — see [LICENSE](https://github.com/meteora-pro/devboy-tools/blob/main/LICENSE).
