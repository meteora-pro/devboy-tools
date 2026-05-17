# Catalog URL sources

`devboy-tools` ships with bundled token catalogs (`kimi`, `openai`, `github`) and reads any team-authored catalogs from `~/.devboy/secrets/catalog/` and `<project>/.devboy/secrets/catalog/`. **URL sources** are the fourth tier: a `sources.toml` file that lets the loader pull provider catalogs straight from a URL — typically a `raw.githubusercontent.com` link to a team-shared repo.

This is powerful (one canonical procedure for every team without copying JSON into every checkout) and dangerous (the URL controls every label, hyperlink, regex, and liveness endpoint the GUI shows) — so URL sources are **opt-in** and ship with five defence layers in front of them.

> Authoring the JSON itself: see [`token-catalog.md`](./token-catalog.md). This document covers serving and consuming that JSON over the network.

> Architectural background: ADR-023 §3.4 (UX layer) + the project memory `project_url_catalog_design.md`.

## Threat model

When you point the loader at a URL, you are trusting whoever controls that URL to be honest about:

- **Where to obtain a token** (`retrieval.console_url`) — a hostile catalog could substitute a phishing console.
- **What shape the token must have** (`format_regex`) — mostly UX, but a permissive regex can mask a typo'd value.
- **Where to send the typed token for liveness** (`liveness.url`) — this is the most dangerous: the user types a real, fresh secret into the GUI, and the catalog tells the GUI where to ship it. A `liveness.url` of `http://attacker.invalid/log` would exfiltrate every token typed.

The five defence layers below address these in turn.

## Defence layers (in order applied)

### 1. Trust establishment

- **HTTPS-only.** The loader refuses any `http://` URL — even when written into `sources.toml` directly. There is no opt-out.
- **SHA256 pin.** `[[source]].sha256 = "abc..."` in `sources.toml` is compared against the body the loader hashed. Mismatch → `BlockedPin`, refused. Use this when you control the upstream and want zero ambiguity.
- **TOFU (trust-on-first-use).** When `sha256` is not pinned, the loader records the body's SHA256 in `~/.devboy/secrets/catalog/known_hashes.toml` on first successful fetch. Every subsequent fetch must match. Mismatch → `BlockedTofuMismatch`, refused. Same pattern as SSH `known_hosts`.
- **GUI confirm before recording (P23.6).** The GUI launches with `RequireConfirmation` policy: the very first fetch from a new URL is paused with a confirm dialog showing the URL + SHA256, and only writes to `known_hashes.toml` once the user clicks "Trust this catalog". The CLI uses `AutoRecord` (unattended).

### 2. Content guards

- **JSON Schema 2020-12** (`crates/devboy-token-catalog/schema/v1.json`) plus `deny_unknown_fields` — unrecognised keys fail the load instead of being silently ignored.
- **256 KB body cap** (`MAX_CATALOG_BODY_BYTES`). Checked twice — once via `Content-Length` before the body comes down, once after, so a server omitting the header can't bypass it. → `BlockedSize`.
- **`Content-Type: application/json`** required. A server quietly returning a login page (HTML) on auth-expiry is rejected outright. → `BlockedContentType`.
- **Schema version match** — the bundled binary refuses bodies with a `schema_version` it doesn't know how to interpret. → `BlockedSchemaVersion`.

### 3. SSRF guard

The catalog declares both `retrieval.console_url` and `liveness.url`. The latter is where the GUI ships freshly-typed secrets. If a hostile catalog points the liveness URL at private infrastructure (`http://10.0.0.5/log`, `http://169.254.169.254/...` for AWS metadata, `http://localhost:9090/`), every token a user types lands in the wrong hands.

`check_ssrf_safe` resolves the hostname to every IP it would dial and refuses if any one falls into:

- **IPv4**: loopback (`127.0.0.0/8`), private (`10/8`, `172.16/12`, `192.168/16`), link-local (`169.254/16`), broadcast, unspecified, multicast.
- **IPv6**: loopback (`::1`), unspecified (`::`), multicast, ULA (`fc00::/7`), link-local (`fe80::/10`).
- **Cloud-metadata hostnames**: `metadata.google.internal`, `metadata.aws.internal`, `metadata.azure.com`, `metadata`, `169.254.169.254` — refused on hostname before DNS, then re-checked on resolved IPs (defence against a hostile DNS resolving them to public addresses).

The same guard fires:
- on every **liveness probe** (rust-catalogue and catalog-driven paths),
- on the **catalog URL itself** before fetching it (P23.7).

### 4. Disk cache + offline behaviour

- Cached body lives at `~/.devboy/secrets/catalog/cache/<sha256-of-url>.json`, sidecar metadata at `<sha256-of-url>.meta.toml` (URL, body sha256, ETag, fetched_at).
- Within `refresh_seconds` (default 24 h) the loader skips the network entirely and serves from cache.
- Past TTL the loader sends `If-None-Match: <stored ETag>`. A 304 reuses the cached body; a 200 replaces it.
- **Offline graceful fallback**: if the network throws, a stale cached body is served as a degraded best-effort.
- **Tamper resistance**: every cache read re-hashes the body and compares to `meta.sha256` — a tampered file is treated as a cache miss and the loader refetches.

### 5. UX-side defence

- **Source chip** in the GUI: orange `[url:host]` next to the variant title. Always-on visible distinction from the gray `[bundled]`, blue `[user]`, and green `[project]` chips.
- **First-fetch confirm dialog**: shows the full URL + SHA256 + an explicit "Trust this catalog" / "Reject" choice.
- **SHA-mismatch warning dialog**: red header, cautionary copy ("most often a legit rotation, but exactly what an upstream compromise looks like"). User can choose "Trust the new SHA" (overwrites `known_hashes.toml`) or "Reject and keep old SHA".
- **Audit log** (see below).

## Opt-in flag

URL sources are gated behind a master switch in `sources.toml`:

```toml
enable_url_catalogs = true
```

When `enable_url_catalogs` is `false` (the default) — even with `[[source]]` blocks present — the loader silently skips every URL entry. A careless paste of someone else's `sources.toml` does **not** auto-activate network fetches.

## Authoring `sources.toml`

The file lives at `~/.devboy/secrets/catalog/sources.toml`:

```toml
enable_url_catalogs = true

# Pinned: zero ambiguity, fails closed on any change.
[[source]]
url = "https://raw.githubusercontent.com/your-team/devboy-catalog/main/anthropic.json"
sha256 = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
refresh_seconds = 86400

# TOFU: convenient for early adoption, prompts on every drift.
[[source]]
url = "https://raw.githubusercontent.com/your-team/devboy-catalog/main/internal-llm.json"
refresh_seconds = 3600
```

### Field reference

| Field | Required | Notes |
|---|---|---|
| `url` | yes | Must start with `https://`. Validated by `parse_sources_toml`. |
| `sha256` | no | Lowercase hex, exactly 64 chars. When set, switches the source from TOFU to pinned. |
| `refresh_seconds` | no | Cache TTL. Default 24h. Lower for fast-moving entries; never below ~60s in practice (you'll just hammer the upstream). |

Unknown TOML keys fail the parse (`deny_unknown_fields`).

## Verifying a remote catalog matches a published sha256

1. Download the JSON manually:
   ```sh
   curl -sSfL https://raw.githubusercontent.com/your-team/devboy-catalog/main/anthropic.json -o /tmp/anthropic.json
   ```
2. Hash it:
   ```sh
   sha256sum /tmp/anthropic.json
   ```
3. Compare against the value the upstream published (commit message, release notes, signed announcement).
4. Apply the verified SHA — either at add-time:
   ```sh
   devboy secrets catalog add-url https://.../anthropic.json --pin <sha256>
   ```
   or, for a URL already subscribed:
   ```sh
   devboy secrets catalog pin https://.../anthropic.json <sha256>
   ```
   Both write the SHA into the canonical state files; neither requires editing `sources.toml` / `known_hashes.toml` by hand.

For a team operating its own catalog: ship the SHA256 in your release notes / CI artifact metadata so consumers can pin without trust-on-first-use.

## Audit log

Every URL fetch attempt — successful or refused — appends one JSONL line to `~/.devboy/secrets/catalog/audit.log`:

```json
{"timestamp":"2026-05-10T12:34:56+00:00","url":"https://...","status_code":200,"sha256":"abc...","outcome":"loaded","detail":""}
```

Outcomes (one of):

| Outcome | Meaning |
|---|---|
| `loaded` | Fetched fresh + activated. |
| `loaded-from-cache` | Within-TTL cache hit, no network. |
| `served-stale-cache` | Network failed, served stale cached copy. |
| `first-fetch-pending` | RequireConfirmation policy, waiting on user. |
| `blocked-pin` | `sources.toml` SHA256 didn't match. |
| `blocked-tofu-mismatch` | `known_hashes.toml` SHA256 didn't match. |
| `blocked-ssrf` | URL host / IP refused by SSRF guard. |
| `blocked-size` | Body exceeded 256 KB cap. |
| `blocked-https-required` | `http://` URL refused. |
| `blocked-content-type` | Body wasn't `application/json`. |
| `blocked-http-status` | Server returned non-2xx, non-304. |
| `blocked-schema-version` | Body's `schema_version` not supported. |
| `blocked-parse` | Body wasn't valid JSON / `ProviderCatalog`. |
| `network-error` | TCP / DNS / TLS failure with no cached fallback. |

Best-effort writes: a disk-full audit log never blocks the catalog load. `tail -f ~/.devboy/secrets/catalog/audit.log | jq` is the canonical incident-response inspection command.

## Recovering from a SHA mismatch

When the GUI surfaces a TOFU mismatch warning (or the CLI logs `blocked-tofu-mismatch`):

1. **Verify out-of-band that the upstream rotated legitimately.** Release notes, CI artifact, signed announcement — anything that's not the same channel that's now serving the new body.
2. If the rotation is confirmed: in the GUI, click "Trust the new SHA" — `known_hashes.toml` is overwritten and the catalog activates. From the CLI, use one of the dedicated subcommands instead of editing TOML by hand:
   - `devboy secrets catalog forget <url>` — drops both the `[[source]]` and the recorded SHA, so the next `refresh` (or implicit re-fetch) restarts the TOFU first-fetch flow with the new body.
   - `devboy secrets catalog pin <url> <sha256>` — overwrites the pinned SHA after you've verified the new value out-of-band. Safer than `forget` when you want to lock the rotation.
   - `devboy secrets catalog refresh <url>` — force a re-fetch with the existing pin / TOFU record (useful after a `pin`).
3. If you cannot confirm: refuse and audit. The cached copy still works for the duration of `refresh_seconds`, so you have at least one TTL window to investigate before the GUI keeps complaining.

## See also

- [`token-catalog.md`](./token-catalog.md) — authoring the JSON catalog files, plus the recommended layout for sharing them across a team or community via a Git repo.
- [`onboarding.md`](./onboarding.md) — first-run setup of the secret framework.
- [`agent-protocol.md`](./agent-protocol.md) — MCP-side surface that consumes the catalog.
- ADR-023 §3.4 — UX layer architecture (provision dialog driven by the catalog).
- Issue #258 — token reference repo + crawler (the upstream side of this consumer).
