# Changelog

All notable changes to `devboy-tools` are recorded here. Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/); this project does not yet pin to semantic versioning, so the **Unreleased** section accumulates work between tags and the next minor bump turns it into a dated release.

## [Unreleased]

### Changed — crates.io publishing policy (#308)

- crates.io now ships **only the reusable libraries**. `devboy-cli` (the app
  binary) and `devboy-mcp` are `publish = false` — they hard-depend on the
  internal secrets plugins, so they're distributed via npm (`@devboy-tools/cli`)
  + prebuilt release binaries, not `cargo install`. The release now derives its
  publish set + order from `cargo metadata` (no hardcoded crate list) and a
  `cargo publish --workspace --dry-run` CI gate catches packaging / metadata /
  unpublishable-dependency errors before tagging — fixing the chronic
  crates.io release failures.

## [0.32.0] - 2026-07-25

### Security / Dependencies

- Eliminate all RustSec advisory ignores (#308). Bump `ratatui` 0.29 → 0.30
  (drops the unmaintained `paste`, RUSTSEC-2024-0436) and `keepass` 0.12 → 0.13
  (which requires `quick-xml >= 0.41`); `wayland-scanner` (transitive, via
  eframe) now resolves to >= 0.31.11, also on `quick-xml >= 0.41` — together
  fixing the RUSTSEC-2026-0194 / -0195 DoS advisories. `deny.toml`
  `[advisories].ignore` is now empty. No first-party API/behavior changes.

### Added — OAuth 2.1 for proxy MCP upstreams (#307)

`auth_type = "oauth2"` proxies now authenticate via the OAuth 2.1 **device
authorization grant** (RFC 8628): `devboy login <server>` discovers the
authorization server from the upstream's `WWW-Authenticate` challenge
(RFC 9728 → RFC 8414), registers a public client (RFC 7591), prints a code +
URL to approve in a browser, and stores **auto-refreshing** tokens.

- Per-request Bearer injection + **refresh-on-401** (single retry) on both
  streamable-http and SSE transports; single-flight, store-reconciled refresh
  that survives the DevBoy AS's rotating refresh tokens.
- **RFC 8707 resource-indicator** audience binding — the token is scoped to the
  MCP server it was issued for.
- SSRF-guarded discovery: only `https`, or `http` to a genuine loopback host;
  IPv6-safe `.well-known` construction; no redirect-following on the token
  endpoint.
- `devboy doctor` reports oauth2 login state by verifying the stored blob
  deserializes as `OAuthTokens` (not mere secret presence).

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

### Changed — GUI lives in its own binary (epic #247 polish)

The eframe/egui rendering stack moved out of the `devboy` CLI into a new
companion binary `devboy-secrets-ui` (crate `devboy-secrets-ui-bin`). The
CLI now spawns the GUI as a subprocess (`find_ui_binary` → `Command::status`),
mirroring the `devboy-secrets-agent` discovery pattern (env override
`DEVBOY_UI_BIN` → sibling of `current_exe()` → `PATH`).

Final sizes on macOS (release + LTO + strip, `panic = unwind` for Sentry):

| Binary | Bytes | MiB | vs baseline |
|---|---|---|---|
| `devboy` (baseline, pre-split) | 27,595,560 | 26.32 | — |
| `devboy` (after split) | **19,428,160** | **18.53** | **−8,167,400 (−29.6%)** |
| `devboy-secrets-ui` (new) | 9,217,616 | 8.79 | new |
| `devboy-secrets-agent` (unchanged) | 1,332,080 | 1.27 | — |

What this gives users:

- **Smaller CLI** for the CI / headless majority. Stripped + LTO'd
  `devboy` drops from **27.6 MiB → 19.4 MiB** (−7.8 MiB). Anyone hitting
  `secrets list`, `secrets validate`, the MCP server, or the format
  pipeline never links eframe, egui, winit, glow, wayland, x11, skrifa
  font rasterizer, or the bundled image decoders.
- **TUI stays inline** — `devboy secrets ui --tui` keeps the ratatui path
  fully inside `devboy`; only `--gui` exec's into the companion.
- **No artifact-naming change** — both binaries ride in the same
  per-platform tarball / zip published to GitHub Releases, and both are
  shipped through the same `@devboy-tools/<platform>` npm packages.

Tradeoff: total on-disk footprint for GUI users is now ~28.6 MiB (19.4
CLI + 9.2 UI) versus the old 27.6 MiB single binary — slight bump
because each binary embeds its own copy of std + tokio + serde, but the
CI win dominates the typical install profile.

### Added — KDBX 4 (KeePass) read-only backend + inventory UX (PR #255)

End-to-end read support for KeePass `.kdbx` files as a `SecretSource`,
plus search + hierarchical-tree UX in the inventory view to make a
~100+ entry KeePass DB navigable.

**KDBX plugin** ([`crates/plugins/secrets/kdbx`](crates/plugins/secrets/kdbx/)):

- Built on the [`keepass = "0.12"`](https://docs.rs/keepass) crate. KDBX 4
  (Argon2id KDF + ChaCha20-Poly1305 / AES-256 + HMAC-SHA256) read path
  with optional keyfile two-factor unlock.
- `KdbxSource` implements `SecretSource` per ADR-021 §8 with capabilities
  `READ | LIST | BIOMETRIC_PROMPT`. Read-only MVP — write is a future
  follow-up (KeePass-GUI concurrent-write safety design needs its own
  RFC).
- Path mapping: `Personal/Cloud/AWS Access Key` → `kdbx/personal/cloud/aws-access-key`
  (lowercased, `[a-z0-9_-]` only, `/`-separated). Single-segment or
  two-segment KeePass entries are namespaced under `kdbx/imported/<x>`
  to satisfy ADR-020's 3-segment minimum.
- Full per-entry metadata extraction: Title, UserName, Password, URL,
  Notes, **UUID** (stable id), **tags**, **created/modified/expires**
  timestamps, **raw OTP source**, **all custom string fields** (both
  Protected and Unprotected), **attachment names + sizes** (bytes are
  never read into the snapshot).
- Auto-detect value field: standard Password wins; falls back to the
  unique Protected custom string when Password is empty (many users
  park API tokens in a custom `api_token` field). `ValueField` enum
  records which won; UI shows a "(value field: custom string `<name>`)"
  note in the context card.
- Agent-blindness boundary per ADR-023 §3.7: the decrypted snapshot
  lives ONLY inside the `devboy-secrets-ui` process. The daemon never
  opens the KDBX file. The `secrets list` / MCP `secrets_describe`
  surface sees titles + URLs but never values.

**UI integration** ([`crates/devboy-secrets-ui-bin`](crates/devboy-secrets-ui-bin/)):

- New `StorageBackend::KdbxLocked` + `Kdbx` variants. Selected via
  `DEVBOY_KDBX_FILE` env var (optionally `DEVBOY_KDBX_PASSPHRASE` +
  `DEVBOY_KDBX_KEYFILE`). When locked, the existing vault-unlock modal
  appears with KDBX-specific copy ("Unlock KeePass database" + the file
  path + agent-blindness reminder).
- `load_inventory_or_empty` extended: every KDBX entry that isn't
  already in the project manifest becomes an `InventoryRow` with
  source-label `kdbx` and a full `IndexEntry` projection. Notes / tags
  / custom string keys / attachment names / UUID / OTP marker all land
  in the context card.

**Inventory UX** ([`crates/devboy-secrets-ui`](crates/devboy-secrets-ui/)):

- **Search bar** above the inventory (🔍 input with hint text, real-time
  filter, ✕ clear button, "N of M shown" counter, `Cmd+F` / `Ctrl+F`
  focus shortcut). Substring-matches case-insensitively across
  `path / scope / provider / catalog_override / routed_source`.
  Deliberately excludes `expires_at` and per-row metadata so a query
  can't accidentally leak rotation-date patterns or values.
- **Hierarchical tree view** built from path segments. Default for any
  inventory with ≥20 rows; toggle between Flat / Tree at any time.
  Per-group header shows `(N)` leaf-count summary. User-expanded
  prefixes persist in `InventoryState.expanded`. With a non-empty
  query, every ancestor of every matching leaf is force-expanded so
  results surface without click-through.
- **CLI fallback**: new `devboy secrets kdbx peek --file <path>`
  subcommand for headless verification — secure passphrase prompt
  via `dialoguer::Password` (no echo, no shell history), prints the
  inventory table (path + Title + UserName + URL + `password?` yes/no),
  values NEVER printed. `--json` flag emits JSON-lines per entry.
- **Attachment download** (K21): KDBX entries with files attached
  (PEM keys, screenshots, `.keytab`, etc.) now show an "Attachments"
  section in the provision dialog with one row per file (name +
  human-readable size + 💾 Save). Save opens the native OS picker
  (`NSSavePanel` / GTK `xdg-desktop-portal` / `IFileSaveDialog` via
  the `rfd` crate), then re-derives the KDBX body with the cached
  passphrase, walks to the entry by UUID, and writes the bytes to
  the chosen path. Bytes are never cached in the inventory snapshot
  — extraction is on-demand and stays inside the `devboy-secrets-ui`
  process address space (agent-blindness boundary preserved).

### Added — KDBX metadata write-surface + CLI + MCP (K14-K17)

Lets agents rotate documentation around a KDBX entry —
notes, tags, expiry timestamp, title, username, url — without
ever touching the value-bearing Password or any Protected
custom string. The ADR-023 §3.7 agent-blindness boundary is
enforced at THREE layers:

* The plugin's `MetadataPatch` struct has no field for the
  Password or Protected fields — there is literally no API
  surface to mutate them through this flow.
* `describe_metadata` returns the same shape minus values —
  agents see what they're allowed to change, no more.
* The MCP wrapper reads the KDBX passphrase from the
  `DEVBOY_KDBX_PASSPHRASE` env var; the agent never sees it
  on the wire.

Write-side safety: every edit lands on a sibling working-copy
(`<source>.devboy-working-<UTC>.kdbx`) derived through K13's
existing `derive_working_copy_path` / `prepare_working_copy`
helpers. The user's original `.kdbx` is never overwritten;
sync-back is left to the caller.

**Plugin** ([`crates/plugins/secrets/kdbx`](crates/plugins/secrets/kdbx/)):
- `MetadataPatch` — all-optional struct (title, username, url,
  notes, tags, `expires_at: Option<Option<String>>` for the
  three-state set / clear / leave).
- `KdbxEntryMetadata` — read-only projection with the same
  fields plus `uuid`, `created_at`, `modified_at`, `otp`
  (None or "(present)"), `attachments` metadata, and
  `custom_string_names`.
- `edit_metadata(file, passphrase, keyfile, uuid, &patch)` —
  opens, mutates, `Database::save` back. Writes verbatim to
  the path passed in (working-copy enforcement is the caller's
  job). Returns `KdbxSourceError::OpenFailed` on wrong
  passphrase / corrupt body / unknown UUID / bad expires_at.
- `describe_metadata(file, passphrase, keyfile, uuid)` — read
  companion; returns `Ok(None)` for unknown UUID.
- 7 new unit tests covering round-trip notes/tags/url,
  expires_at set + clear, bad-expires-at rejection,
  unknown-UUID rejection, describe field projection,
  describe None-for-unknown-UUID.
- `keepass = "0.12"` `save_kdbx4` feature promoted from
  dev-only to unconditional.

**CLI** ([`crates/devboy-cli/src/secrets_cmd.rs`](crates/devboy-cli/src/secrets_cmd.rs)):
- `devboy secrets kdbx describe-metadata --file <path>
  --uuid <hex> [--keyfile <p>] [--json]` — secure passphrase
  prompt via `dialoguer::Password`, prints the metadata
  projection as a key/value table or `--json`.
- `devboy secrets kdbx edit-metadata --file <path>
  --uuid <hex> [flags]` — flags: `--title`, `--username`,
  `--url`, `--notes` (empty string clears each), `--tag
  <name>` repeatable, `--clear-tags`, `--expires-at <RFC-
  3339>`, `--no-expiry`. Refuses no-op invocations. Writes
  to working-copy; prints the path on success.

**MCP** ([`crates/devboy-mcp/src/server.rs`](crates/devboy-mcp/src/server.rs)):
- `kdbx_describe_metadata { file, uuid, keyfile? }` and
  `kdbx_edit_metadata { file, uuid, keyfile?, patch }` tools.
  Passphrase via `DEVBOY_KDBX_PASSPHRASE` env var; refuses if
  missing / empty. `patch.expires_at` is three-state via a
  `deserialize_double_option` serde helper (missing /
  null / set). Both registered in `is_internal_tool` so the
  MCP speculation engine never pre-executes them — every
  write is explicit and observable.

**Docs**: this CHANGELOG entry + `crates/plugins/secrets/kdbx/
README.md` write-surface section + regenerated
`docs/guide/reference/cli.md` so the drift-check test sees
the new subcommands.

### Added — Approve-on-use cron-mode operations work (PR #255 follow-ups)

After the main epic landed, the PR captured four Codex review finds (F1
SSRF redirect bypass, F2 add-url confirm-before-write, F3 SessionApprovalCache
wired into proxy_secrets + argv_secrets, F4 ProductionWizardIo provision via
daemon RPC + `load_all_with_urls` in the proposer), two nits
(`RequestIdReply` typed wrapper on 5 MCP handlers + extended leak test;
cli.md `summary` vs `message` drift + replaced catalog-url-sources.md
hand-edit guidance with `catalog forget` / `pin` / `refresh`), and five
coverage-boost test series totaling 70+ new tests across catalog CLI,
secrets_setup, secrets_validate, doctor/checks/sources, and the
secrets_migrate + secrets_rotate happy paths.
