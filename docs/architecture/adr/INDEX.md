# Architecture Decision Records

This directory holds Architecture Decision Records (ADRs) for `devboy-tools`. Each ADR captures a single architectural decision — the context that led to it, the decision itself, its consequences, and the alternatives that were rejected.

## Index

| #   | Title | Status | Scope |
|-----|-------|--------|-------|
| [001](./ADR-001-apache-license.md) | Apache 2.0 license for the project | accepted | Legal |
| [002](./ADR-002-rust-architecture.md) | Rust-based architecture with npm binary distribution | accepted | Core |
| [003](./ADR-003-testing-strategy.md) | Testing strategy — layered mocking with optional record-and-replay for real APIs | accepted | Testing, CI |
| [004](./ADR-004-trait-based-mocking.md) | Trait-based provider abstraction for testability and extensibility | accepted | Core |
| [005](./ADR-005-credential-storage.md) | Credential storage — OS keychain with environment-variable fallback | accepted | Storage, Security |
| [007](./ADR-007-plugin-architecture.md) | Plugin architecture — API providers, format pipeline, and the enricher model | accepted | Plugins |
| [010](./ADR-010-asset-management.md) | Asset management — file attachments for AI agents | accepted | Assets (phases 1–3 shipped; phase 5 pending) |

**Number gaps** (006, 008, 009, 011, …) are intentional. Those numbers are reserved for decisions that are not in scope for this project.

## Writing a new ADR

1. Copy [`TEMPLATE.md`](./TEMPLATE.md) to `ADR-NNN-short-title.md` using the next available number
2. Fill in every section (Context → Decision → Consequences → Alternatives → Implementation → References)
3. Set `status: proposed` initially. Flip to `accepted` once the decision has been implemented (or rejected / superseded if it turns out the other way)
4. Add the new ADR to the table above
5. Open a PR

## Conventions

- **English only.** All ADRs in this directory are written in English.
- **Single decision per ADR.** If a file is trying to describe two decisions, split it.
- **Status reflects reality.** If code on `main` no longer matches an `accepted` ADR, either update the code or mark the ADR `superseded_by`.
- **No implementation dumps.** Code samples should illustrate the decision, not serve as a mini-copy of the source tree.
- **Write for future readers.** Someone joining the project a year from now should be able to read an ADR and understand both what was decided and why alternative paths were not taken.

## Further reading

- [Documenting Architecture Decisions](https://cognitect.com/blog/2011/11/15/documenting-architecture-decisions.html) — the original Michael Nygard post
- [MADR template](https://adr.github.io/madr/)
- [adr.github.io](https://adr.github.io/)
