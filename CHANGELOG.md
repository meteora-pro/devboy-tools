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
