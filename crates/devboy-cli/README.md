# devboy-cli

[![Crates.io](https://img.shields.io/crates/v/devboy-cli.svg)](https://crates.io/crates/devboy-cli)
[![Docs.rs](https://docs.rs/devboy-cli/badge.svg)](https://docs.rs/devboy-cli)
[![License](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](https://opensource.org/licenses/Apache-2.0)
[![npm](https://img.shields.io/npm/v/@devboy-tools/cli)](https://www.npmjs.com/package/@devboy-tools/cli)

The `devboy` CLI binary for [`devboy-tools`](https://github.com/meteora-pro/devboy-tools).

## Install

The **primary** distribution is npm — that's what `devboy doctor`, `devboy onboard`, and the Claude Code / Codex plugins all assume:

```bash
npm install -g @devboy-tools/cli
```

`cargo install` is a **secondary** channel, useful when you already have a Rust toolchain and want to skip Node:

```bash
cargo install devboy-cli --locked
```

The two channels ship the same binary from the same release tag.

## Documentation

- [User guide](https://github.com/meteora-pro/devboy-tools#readme)
- [Tools reference](https://github.com/meteora-pro/devboy-tools/blob/main/docs/guide/reference/tools.md)
- API reference on [docs.rs/devboy-cli](https://docs.rs/devboy-cli)

## License

Apache-2.0 — see [LICENSE](https://github.com/meteora-pro/devboy-tools/blob/main/LICENSE).
