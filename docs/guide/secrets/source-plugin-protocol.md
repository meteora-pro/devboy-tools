# Source plugin protocol: writing your own `SecretSource`

A guide for community plugin authors who want to extend the list of secret sources in `devboy-tools` beyond what ships in the box (keychain, local-vault, 1Password, Vault, env-store). Covers the stdio JSON-RPC wire format, the sidecar manifest, discovery, and the lifecycle contract.

See [ADR-021](../architecture/adr/ADR-021-secret-source-router.md) §6 (the `SecretSource` trait) and §10 (subprocess plugin extension), plus `crates/devboy-storage/src/plugin_protocol.rs`, `plugin_manifest.rs`, and `plugin_client.rs` for the reference host implementation.

> Russian translation: [`ru/source-plugin-protocol.md`](./ru/source-plugin-protocol.md).

---

## Why this exists

The standard sources cover typical setups, but if your team stores secrets in:

- an internal self-hosted KV server that doesn't speak the Vault HTTP API,
- a proprietary HSM with a CLI wrapper,
- a legacy pipeline that pulls values from a domain-specific DB,

it's faster to ship a subprocess plugin than wait for an upstream feature. The host launches the plugin lazily and talks to it over stdio JSON-RPC; the plugin can be in any language (Python, Go, shell — anything that reads stdin line-by-line and writes to stdout).

## High-level architecture

```text
┌──────────────────────────────────┐
│ devboy host (CLI / daemon / MCP) │
└──────────────────────────────────┘
                 │
                 ▼ lazy spawn on first request
┌──────────────────────────────────────────────────┐
│ ~/.devboy/plugins/secrets/devboy-source-<name>   │
│  ↑↓  newline-delimited JSON-RPC over stdin/stdout│
└──────────────────────────────────────────────────┘
                 │
                 ▼
       real backend (HSM, custom KV, …)
```

The host keeps one subprocess per plugin alive for the session (with an idle reaper — see lifecycle), sends commands over stdin, reads replies from stdout. No network sockets, no FFI — only a pipe.

## Wire format: JSON-RPC 2.0 over stdin/stdout

Each frame is one line containing one JSON object, terminated by `\n`. Requests and replies are strictly serial (one request → one reply; no concurrency).

### Request (host → plugin)

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "secret_source.init",
  "params": {
    "source_name": "<name>",
    "config": { "address": "https://example.invalid/<service>" },
    "protocol_version": "1.0"
  }
}
```

- `jsonrpc` — always the string `"2.0"`. Any other value must reply with an error.
- `id` — integer (u64). The host guarantees uniqueness within a single process.
- `method` — one of the five names below.
- `params` — method-specific object. Empty object (`{}`) or omitted for parameter-less methods.

### Successful reply (plugin → host)

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "result": {
    "source_name": "<name>",
    "capabilities_bits": 1,
    "plugin_version": "0.1.0"
  }
}
```

`id` must echo the request's `id`. The host checks and aborts if they don't match.

### Error reply

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "error": {
    "kind": "needs-credential",
    "detail": "op signin required for account <id>"
  }
}
```

`result` and `error` are mutually exclusive — exactly one is present in each reply.

## The five methods

### `secret_source.init`

The first call after spawn. The host hands the source name and config from `sources.toml`; the plugin returns its capabilities.

**Request params**:

```json
{
  "source_name": "<name>",
  "config": { "<arbitrary>": "<config>" },
  "protocol_version": "1.0"
}
```

**Response result**:

```json
{
  "source_name": "<name>",
  "capabilities_bits": 1,
  "plugin_version": "<version>"
}
```

`capabilities_bits` is a bit mask (see `crates/devboy-storage/src/source.rs::Capabilities`):

| Bit | Capability | What you're claiming |
|---|---|---|
| `0b0000_0001` | `READ` | Supports `secret_source.get` |
| `0b0000_0010` | `LIST` | Supports `secret_source.list` |
| `0b0000_0100` | `VALIDATE` | Supports `secret_source.validate` |
| `0b0000_1000` | `WRITE` | Reserved (registered, not yet used) |
| `0b0001_0000` | `ROTATE` | Reserved |
| `0b0010_0000` | `BIOMETRIC_PROMPT` | Source may prompt for biometrics (1Password Touch ID) |
| `0b0100_0000` | `AUDIT_LOGGED` | Source writes an audit log on every access |

If the plugin only supports READ — send `"capabilities_bits": 1`.

The host refuses to continue if the request's `protocol_version` is older than the plugin understands at the major version.

### `secret_source.is_available`

Readiness check. Cheap operation, must return quickly (< 100 ms is the target).

**Request params**: none (empty object `{}` or omitted).

**Response result**:

```json
{
  "status": "available",
  "detail": null
}
```

Possible `status` values:

- `available` — plugin is ready to answer `get` / `list` / `validate`.
- `unavailable` — backend down (network down, file corrupt). `detail` — short description for `doctor`.
- `needs-credential` — plugin is up, but its credential is missing or expired (`op signin` required). `detail` — exactly what to do.

### `secret_source.get`

Fetch a value by reference.

**Request params**:

```json
{ "reference": "secret/data/team/<provider>/api-key" }
```

**Response result**:

```json
{
  "value": "<plaintext>",
  "lease_seconds": 3600
}
```

`value` is plaintext. **This is the only place in the protocol where a value crosses the wire.** The host wraps it in `secrecy::SecretString` immediately and zeroizes after use. The plugin must not log `value`.

`lease_seconds` is optional — for backends with leases (Vault dynamic secrets). The host uses it as the upper bound for its cache TTL.

If the value isn't there, return `error.kind = "bad-reference"` with `reference: "<reference>"` and `reason: "not found"`.

### `secret_source.list`

List the backend's inventory. Used by the discovery flow in TUI/GUI and by the `secrets_propose_new_path` MCP tool.

**Request params**: none.

**Response result**:

```json
{
  "entries": [
    {
      "reference": "secret/data/team/<provider>/api-key",
      "display": "Team <provider> API key"
    },
    {
      "reference": "secret/data/team/<provider>/deploy-token",
      "display": null
    }
  ]
}
```

If the backend doesn't support enumeration, return `error.kind = "unsupported-capability"` with `capability: "list"`.

### `secret_source.validate`

Check that a reference is well-formed without round-tripping for the value. Cheap sanity check.

**Request params**:

```json
{ "reference": "secret/data/team/<provider>/api-key" }
```

**Response result**:

```json
{ "ok": true }
```

`ok = false` is unusual; report errors via `error.kind = "bad-reference"` instead.

## Error variants

```json
{ "kind": "unavailable",            "detail": "<...>" }
{ "kind": "unsupported-capability", "capability": "<list|write|...>" }
{ "kind": "bad-reference",          "reference": "<r>", "reason": "<...>" }
{ "kind": "needs-credential",       "detail": "<...>" }
{ "kind": "other",                  "detail": "<...>" }
```

The host maps these one-to-one onto its `SourceError` enum. `other` is a catch-all for anything that doesn't fit the others (transport timeout, parse error, …).

## Sidecar manifest

Every plugin sits next to a TOML descriptor:

```text
~/.devboy/plugins/secrets/
├── devboy-source-<name>.toml      # sidecar manifest
└── devboy-source-<name>           # executable
```

### `devboy-source-<name>.toml` format

```toml
name = "<name>"
version = "0.1.0"
executable = "devboy-source-<name>"
allowed_env_vars = ["HOME", "PATH"]
checksum_sha256 = "<lower-case-hex-digest-of-executable>"
```

Fields:

- `name` — short identifier. Must equal `<name>` in the manifest filename — the host refuses to load otherwise.
- `version` — advisory; logged and shown in `doctor`. Not used for semantic compatibility.
- `executable` — path relative to the manifest directory (or absolute). The host canonicalises and checks the file exists.
- `allowed_env_vars` — the only env vars that reach the child process. The host calls `Command::env_clear()` before exec, then injects exactly these variables. Everything else (including `$AWS_SECRET_KEY`) is hidden.
- `checksum_sha256` — SHA-256 hex (case-insensitive) of the executable bytes. The host re-hashes and refuses to launch on mismatch.

### Where it lives

The default discovery directory is `$HOME/.devboy/plugins/secrets/`. The host scans it at startup, looks for files starting with `devboy-source-` and ending in `.toml`. Each manifest parses independently — one broken config does not disable the rest of the plugins.

## Lifecycle contract

See ADR-021 §10 for the full statement. In short:

| Parameter | Default | Description |
|---|---|---|
| **Lazy spawn** | — | The plugin is not launched until the host actually needs the first request. |
| **Idle timeout** | 60 seconds | If unused for longer, the host kills the process before the next request and respawns. |
| **Shutdown grace** | 10 seconds | On stop, the host sends `SIGTERM`, waits the grace, then `SIGKILL`. |
| **Restart cap** | 3 crashes / 60 seconds | Crash above the cap → state `Disabled`. Further requests refuse without spawn. Reset by the operator via `doctor`. |
| **Env restriction** | `allowed_env_vars` from manifest | The child process sees only the listed variables. |

What this means for the plugin author:

- **Don't assume long uptime.** Any long-lived state lives in the external backend (KV server, file), not the process memory. After idle-reap, in-memory state is gone.
- **Init must be cheap.** Every lazy spawn is `init` + first operation. If init reads a 100 MB cache from disk, the user notices a lag on every cold call.
- **Crash safety.** A panic counts as a crash against the restart cap. Catch exceptions in the plugin and return `error.kind = "other"` with a useful `detail` instead of letting the process die.
- **Clean shutdown.** When the host closes your stdin, that's the signal for graceful exit. Release resources and return from main.

## Sample plugin: echo-source

The repo ships a working example at `examples/secrets-source-echo/`. A Python script that:

- Accepts init and claims `READ | LIST | VALIDATE` capabilities.
- On `get`, returns `value = format!("echo:{reference}")` — i.e. value mirrors reference. Convenient for smoke tests and learning the protocol.
- On `list`, returns three fake entries.
- On `validate`, accepts any non-empty reference.

See [`examples/secrets-source-echo/README.md`](../../../examples/secrets-source-echo/README.md) for build + install + smoke-test instructions.

## New-plugin checklist

1. [ ] Implement the main loop: read stdin line-by-line, parse JSON, dispatch by `method`, print one line of JSON reply to stdout.
2. [ ] Cover all five methods or return `unsupported-capability` for ones you don't.
3. [ ] Declare honest `capabilities_bits` in the `init` reply — otherwise the host will ask for things you don't deliver.
4. [ ] Never log the `value` from `secret_source.get`. Never.
5. [ ] Handle signals (SIGTERM / Ctrl+D) cleanly — graceful exit, no hangs.
6. [ ] Write the sidecar TOML with correct `name`, `executable`, `allowed_env_vars`, `checksum_sha256`.
7. [ ] Drop both files into `~/.devboy/plugins/secrets/`.
8. [ ] Run `devboy doctor --secrets`. Your plugin should show up in the source list.
9. [ ] Run `devboy secrets describe --source <name>` (when the flag lands) or trigger the `secrets_request_provision` MCP tool against a path that routes into your plugin.

## See also

- [`onboarding.md`](./onboarding.md) — how to add a custom source to `sources.toml` so the router uses it.
- [`agent-protocol.md`](./agent-protocol.md) — how the agent sees your source through MCP tools.
- [`token-catalog.md`](./token-catalog.md) — JSON catalogs that fill the GUI's "where do I get this token" panel; orthogonal to the source plugin protocol but you may want both.
- [`catalog-url-sources.md`](./catalog-url-sources.md) — serving catalogs over the network with sha-pinning + audit log.
- ADR-021 §6 — formal `SecretSource` trait this wire protocol maps onto.
- ADR-021 §10 — subprocess plugin lifecycle contract with rationale for the defaults.
- `crates/devboy-storage/src/plugin_protocol.rs` — wire-format types in Rust.
- `crates/devboy-storage/src/plugin_manifest.rs` — sidecar parser + checksum verification.
- `crates/devboy-storage/src/plugin_client.rs` — host-side supervisor with lazy spawn / idle reap / restart cap.
