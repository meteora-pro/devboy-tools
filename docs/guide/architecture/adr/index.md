---
title: ADRs
description: Architecture Decision Records for DevBoy tools — every load-bearing technical decision with context, alternatives, and consequences.
---

# Architecture Decision Records

ADRs capture each architectural decision: the context that led to it, the decision itself, the consequences, and the alternatives that were rejected. The source-of-truth lives at [`docs/architecture/adr/`](https://github.com/meteora-pro/devboy-tools/tree/main/docs/architecture/adr) in the repo — this page is a stable index.

## Index

| #   | Title | Status | Scope |
|-----|-------|--------|-------|
| [001](https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-001-apache-license.md) | Apache 2.0 license for the project | accepted | Legal |
| [002](https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-002-rust-architecture.md) | Rust-based architecture with npm binary distribution | accepted | Core |
| [003](https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-003-testing-strategy.md) | Testing strategy — layered mocking with optional record-and-replay for real APIs | accepted | Testing, CI |
| [004](https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-004-trait-based-mocking.md) | Trait-based provider abstraction for testability and extensibility | accepted | Core |
| [005](https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-005-credential-storage.md) | Credential storage — OS keychain with environment-variable fallback | accepted | Storage, Security |
| [007](https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-007-plugin-architecture.md) | Plugin architecture — API providers, format pipeline, and the enricher model | accepted | Plugins |
| [010](https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-010-asset-management.md) | Asset management — file attachments for AI agents | accepted | Assets (phases 1–3 shipped; phase 5 pending) |
| [012](https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-012-skills-subsystem.md) | Skills subsystem — procedural recipes on top of the tool bundle | proposed | Skills |
| [013](https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-013-skills-install-targets.md) | Skill install targets — repo-local default, global and agent-specific overrides | proposed | Skills |
| [014](https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-014-skills-lifecycle.md) | Skills lifecycle — manifest-based install, upgrade, and collision detection | proposed | Skills |
| [015](https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-015-skills-session-traces.md) | Skills self-feedback loop — session trace format | proposed | Skills, Observability |
| [016](https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-016-skills-language-adaptation.md) | Skills language adaptation | proposed | Skills (deferred) |
| [017](https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-017-agent-detection-and-onboard.md) | Agent detection and `devboy onboard` command | proposed | Onboarding, Skills |

**Number gaps** (006, 008, 009, 011) are intentional — those numbers are reserved for decisions that are not in scope for this project.

## Writing a new ADR

See [`TEMPLATE.md`](https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/TEMPLATE.md). Copy it to `ADR-NNN-short-title.md` using the next available number, fill in every section, and start with `status: proposed`. Flip to `accepted` once implemented (or `rejected` / `superseded` if it goes the other way).
