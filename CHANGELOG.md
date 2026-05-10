# Changelog

All notable changes to `devboy-tools` are recorded here. Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/); this project does not yet pin to semantic versioning, so the **Unreleased** section accumulates work between tags and the next minor bump turns it into a dated release.

## [Unreleased]

### Added — Secret management framework (epic #247)

End-to-end first-party secret-management framework per [ADR-020](docs/architecture/adr/ADR-020-secret-manifest-and-alias-resolution.md), [ADR-021](docs/architecture/adr/ADR-021-secret-source-router.md), and [ADR-023](docs/architecture/adr/ADR-023-secret-store-ux-layer.md). Single PR (#255) shipped 67 atomic tasks across 19 phases.

**CLI** —
- `devboy secrets list` / `describe` — inventory + per-path metadata cards (manifest-gated; values never shown).
- `devboy secrets validate` — ADR-020 format check + optional `--liveness` upstream probe.
- `devboy secrets migrate` — interactive flow for legacy keychain entries → ADR-020 paths.
- `devboy secrets rotate <path>` — opens provider URL, destructive-confirm, format-validate, records `last_rotated_at`.
- `devboy secrets ui [--tui|--gui]` — backend autodetection (`$DISPLAY`/`$WAYLAND_DISPLAY` on Linux, OS on macOS/Windows).
- `devboy secrets agent {start,status,install,uninstall}` — local daemon lifecycle + launchd/systemd-user service generators.

**Sources** — keychain, local-vault, 1Password, Vault (HTTP KV v2), env-store; subprocess plugin protocol with sidecar TOML manifest, SHA-256 checksum verification, `allowed_env_vars` env-restriction, and a 60s-idle / 10s-grace / 3-restart-cap supervisor.

**UI** — four MVP views (Inventory / Provision-Rotation / Edit-Metadata / Discovery-Import) with shared view-model. ratatui (TUI) + egui (GUI) backends; pure view-model + thin render layer; 91 view-model tests + 4 egui smoke tests.

**Daemon** — JSON-RPC 2.0 over UNIX socket with idle timeout + zeroize, on-demand spawn from CLI, XChaCha20-Poly1305 vault file (Argon2id passphrase, BIP39 recovery phrase, optional macOS Keychain envelope).

**Agent surface (MCP)** — seven tools in the `secrets_*` family: `list`, `describe`, `request_provision`, `request_rotation`, `propose_metadata`, `propose_new_path`, `poll_status`. Trust boundary enforced by `AgentSafeReply` marker trait, CI grep gate (`tests/no_expose_secret_outside_allowlist.rs`), and sentinel negative test. Provision lifecycle: pending → ok / cancelled / expired / failed with 5-minute TTL.

**Skills** — new `setup-secrets` first-run wizard (8-step idempotent flow with state at `~/.devboy/secrets/setup-state.toml`); existing `setup` skill delegates when project ships a manifest.

**Documentation** — four guides under [`docs/guide/secrets/`](docs/guide/secrets/) (Russian per project doc rule): onboarding, local-vault format + recovery + backup, MCP agent protocol, subprocess source-plugin protocol with a working Python echo-source example in [`examples/secrets-source-echo/`](examples/secrets-source-echo/).

**Tests** — 38 storage + 21 secrets-tool + 13 secrets-provision + 6 plugin-client lifecycle + 5 negative leak tests + 2 grep-gate tests + 4 end-to-end CLI tests across the three deployment modes (desktop=keychain, team=local-vault stub, CI=env-store).

### Added — Approve-on-use protocol (P25, epic #247)

Per-path policy gating *use* (not just provision) of high-stakes secrets.

- `IndexEntry` / `OverrideEntry` gain `approve_on_use: Option<ApproveOnUse>` — `Never` (default) / `Session` / `PerCall`.
- New MCP tool `secrets_request_use_approval(path, reason, ttl_seconds?)` — agent supplies a human-facing reason; user picks `once` / `session` / `denied` from a modeless egui dialog.
- `SecretsListItem` and `SecretsDescribeReply` surface the policy on the wire so agents can pre-warn the user.
- `SessionApprovalCache` in `devboy-core` (the lowest crate) — bridge `From<storage::ApproveOnUse>` for `core::ApproveOnUsePolicy` keeps storage decoupled from the cache.
- Three new GUI states for `ProvisionStatus` alongside the original quintet: `once / session / denied`.

### Added — `setup-secrets` wizard skill + CLI driver (P26, epic #247)

Eight-step idempotent flow per ADR-023 §3.8 plus a CLI-first driver.

- `crates/devboy-skills/skills/00-self-bootstrap/setup-secrets/{SKILL.md,entry.sh}` — markdown skill + state-init shell helper, RustEmbed-discovered automatically.
- `devboy secrets setup [--scan-only|--write-manifest|--resume] [--root <dir>] [--json]` — read-only scan preview by default, opt-in commit / resume modes.
- `secrets_setup` library: `scan_repo`, `propose_paths`, `run_wizard`, `read_setup_state` (typed view over `~/.devboy/secrets/setup-state.toml`).
- `WizardIo` trait — production wires real scanner + manifest reader + future MCP-side daemon glue; tests inject `FakeIo`.
- `WizardEvent { PhaseStarted, PhaseProgress, PhaseCompleted, PhaseSkipped, PhaseFailed, Completed }` — both human + JSON-lines render modes.

### Improved — Catalog framework (epic #247 polish)

- `rust-embed` auto-discovery for bundled catalogs — adding a provider is one file drop, zero source changes (was: hardcoded `BUNDLED_SOURCES` array).
- Eight new bundled catalogs (anthropic, clickup, gemini, gitlab, jira, langfuse, ollama, slack) on top of the original kimi / openai / github trio. **11 bundled providers**, covering the full devboy provider-tools surface.
- Schema v1 gains optional `env_var_patterns: Vec<{matches, variant, scope}>` and `env_var_skip: Vec<String>` (additive, `#[serde(default)]`); proposer consults catalog patterns *before* heuristics.
- New CLI commands: `devboy secrets catalog status` (richer than `list` — origin + variant/pattern/skip counts + URL-source state), `add-url`, `refresh [--force]`, `forget [--yes]`, `pin <filter> [<sha>]` — full TOFU-recovery lifecycle.

### Improved — `setup-secrets` proposer accuracy (P1-P5)

Live demo on `meteora/devboy-env-1` (845 env-var references in 123 files) drove a five-step noise reduction:

| Phase | Skip pattern added | Proposed paths |
|---|---|---|
| Baseline | — | 236 |
| P1 | Configuration / connection metadata suffixes (`_BASE`, `_MODEL`, `_LOG`, `_TIMEOUT`, …) | 183 (-22%) |
| P2 | CI runner / build agent / cloud-platform prefixes (`CI_COMMIT_*`, `BUILD_*`, `GITHUB_RUN_*`, `VERCEL_*`, …) | 178 (-25%) |
| P3 | Windows machine env (`COMPUTERNAME`, `APPDATA`, `PROCESSOR_*`, …) | 176 |
| P4 | POSIX locale + XDG + GPG/SSH agent metadata | 176 (preventive) |
| P5 | Ambiguous generic-segment skip (`SERVICE_TOKEN`, `API_KEY`, `BOT_IMAGE` …) | 163 (-31%) |
| post-S2 + bundled catalogs | provider-supplied patterns + skip | **161 (-32%)** |

Real credentials (`OPENAI_API_KEY`, `LANGFUSE_*_KEY`, `GITLAB_ACCESS_TOKEN`, `OLLAMA_API_KEY`, `GEMINI_API_KEY`, `ANTHROPIC_API_KEY`) preserved as path proposals throughout.
