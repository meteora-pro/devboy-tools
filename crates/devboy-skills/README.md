# devboy-skills

[![Crates.io](https://img.shields.io/crates/v/devboy-skills.svg)](https://crates.io/crates/devboy-skills)
[![Docs.rs](https://docs.rs/devboy-skills/badge.svg)](https://docs.rs/devboy-skills)
[![License](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](https://opensource.org/licenses/Apache-2.0)

Skills subsystem for [`devboy-tools`](https://github.com/meteora-pro/devboy-tools) — procedural recipes that AI agents (Claude Code, Codex, OpenCode, Kimi) load on top of the tool bundle.

This crate provides:

- A `SkillSource` trait for pluggable skill backends
- An `EmbeddedSkillSource` that ships baseline `SKILL.md` files inside the binary (via `rust-embed`)
- The `Skill` / `Frontmatter` types, including YAML frontmatter parsing with a fixed required-field set and extensible unknown fields
- Manifest-based install/upgrade lifecycle with collision detection

Implements [ADR-012 — skills subsystem](https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-012-skills-subsystem.md), [ADR-013](https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-013-skills-install-targets.md), [ADR-014](https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-014-skills-lifecycle.md), [ADR-015](https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-015-skills-session-traces.md).

## Features

- `trace` (default) — session-trace module (ADR-015). Pulls in `ulid` for session ids. Disable for a smaller install path.

## Add to your project

```toml
[dependencies]
devboy-skills = "0.26"
```

## Documentation

API reference on [docs.rs/devboy-skills](https://docs.rs/devboy-skills).

For the full bundle see the [main project README](https://github.com/meteora-pro/devboy-tools#readme).

## License

Apache-2.0 — see [LICENSE](https://github.com/meteora-pro/devboy-tools/blob/main/LICENSE).
