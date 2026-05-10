# Agent protocol: secrets through MCP

For authors of AI agents and MCP clients. This document explains the "agent never sees the value" invariant (ADR-023 §3.7), the full list of MCP tools in the `secrets_*` family, their semantics, request lifetimes, and response shapes.

See [ADR-023](../architecture/adr/ADR-023-secret-store-ux-layer.md) §3.7 for the formal spec and `crates/devboy-mcp/src/secrets_tool.rs` + `secrets_provision.rs` for the implementation.

> Russian translation: [`ru/agent-protocol.md`](./ru/agent-protocol.md).

---

## Core invariant

> **The agent never sees secret values. Metadata only.**

This is not a "best practice" or a runtime check — it is a typed boundary in the code. The reply structs of every `secrets_*` tool physically have no `value` field. If a future refactor tries to add a `SecretString` to a reply, compilation fails on the `AgentSafeReply` marker trait. On top of that:

- **CI grep gate** in `crates/devboy-mcp/tests/no_expose_secret_outside_allowlist.rs`. Any `.expose_secret()` outside the allowlist → fail.
- **Negative integration test** in `crates/devboy-mcp/tests/secret_tool_responses_never_leak_value.rs`. Pumps a fixture sentinel through every `secrets_*` tool and asserts the value never appears in the reply.

For the agent side this means: **you cannot write `secrets.get`** and receive a token. If you need a value, it reaches the work through a high-level provider tool (e.g. `get_issues` with the token already wired into a proxy alias). The bypass is available only to the user — through the UI dialog, which never returns a string back to the agent.

---

## Tool list

| Tool name | Purpose | Returns | Lifecycle |
|---|---|---|---|
| `secrets_list` | Browse the inventory of the active context | List of metadata records | Synchronous |
| `secrets_describe` | Detail card for one path | Metadata for one path | Synchronous |
| `secrets_request_provision` | Open the UI dialog for first-time provision | `request_id` | Asynchronous, polling |
| `secrets_request_rotation` | Same with destructive-confirm for replacement | `request_id` | Asynchronous, polling |
| `secrets_propose_metadata` | Suggest metadata edits for an existing path | `request_id` | Asynchronous, polling |
| `secrets_propose_new_path` | Suggest registering a new path in the manifest | `request_id` | Asynchronous, polling |
| `secrets_poll_status` | Get the status of a previously issued `request_id` | `{kind, status, age_seconds, …}` | Synchronous |

UI-dialog requests are asynchronous: the agent kicks off the dialog and polls for status. The user types into the window/TUI at their own pace; the agent only sees the event flow.

---

## `secrets_list`

List the paths declared in the active context's manifest.

### Request

```json
{
  "name": "secrets_list",
  "arguments": {
    "path_contains": "jira",
    "scope": "team",
    "status": "expiring",
    "include_internal": false
  }
}
```

All arguments are optional. Filters AND-combine. `include_internal` defaults to `false` — internal paths (`__sys/...`) are hidden.

### Response

```json
[
  {
    "path": "team/<provider>/api-key",
    "status": "expiring",
    "expires_at": "2026-05-23",
    "source_name": null,
    "capabilities_hint": "read,rotate"
  }
]
```

Fields:

- `path` — ADR-020 path (`<scope>/<provider>/<purpose>`).
- `status` — `registered` / `expiring` (≤14 days to expiry) / `expired`. Computed from `expires_at`, **not** from a live source probe. This is intentional: a probe could accidentally reveal that the user has already revoked the token.
- `expires_at` — ISO-8601 (`YYYY-MM-DD`) or `null`.
- `source_name` — currently always `null` (the router is not exposed to the MCP server; the field is reserved in the wire format).
- `capabilities_hint` — `"read"` (manual rotation) or `"read,rotate"` (provider-ui / provider-api).

The response is sorted by `path` for test stability.

---

## `secrets_describe`

Detail card for one path.

### Request

```json
{
  "name": "secrets_describe",
  "arguments": { "path": "team/<provider>/api-key" }
}
```

### Response

A strict superset of a `secrets_list` element plus extra metadata fields:

```json
{
  "path": "team/<provider>/api-key",
  "status": "registered",
  "expires_at": "2026-12-01",
  "source_name": null,
  "capabilities_hint": "read",
  "description": "API token used by team CI",
  "retrieval_url": "https://example.invalid/<provider>/tokens",
  "rotation_method": "manual",
  "last_rotated_at": "2026-04-15",
  "rotate_every_days": 90,
  "pattern_id": "<pattern-id>"
}
```

Errors:

- `not-found` — path not visible in the active context (neither in the project manifest nor in the global index with overrides).
- `invalid-path` — syntax doesn't match ADR-020.
- `merge-failed` — manifest conflict (duplicate entry, invalid structure).

> **Manifest-gating**: only paths the active project manifest references are visible. The global index is not leaked wholesale.

---

## `secrets_request_provision`

Ask the framework to open a UI dialog where the user types a **new** value for the secret.

### Request

```json
{
  "name": "secrets_request_provision",
  "arguments": {
    "path": "team/<provider>/api-key",
    "mode": "provision"
  }
}
```

`mode` is optional, defaulting to `"provision"`. The value `"rotation"` forces destructive-confirm — but for rotation the dedicated `secrets_request_rotation` (below) is usually clearer.

### Response

```json
{ "request_id": "prov-1f9b3a2c5e7d" }
```

`request_id` is an opaque string of the form `prov-<12-hex>`. Store it and poll via `secrets_poll_status`.

### Lifecycle

```
secrets_request_provision  ──►  request_id, status = pending
                                       │
                                       │  (user types the value into the dialog)
                                       ▼
                                daemon stores value, status = ok
                                       │
secrets_poll_status(request_id) ──►   status = ok
                                       │
agent ──► high-level provider tool (e.g. get_issues),
          token already available through the proxy alias
```

The request expires after **5 minutes** if the user hits neither Save nor Cancel. The agent should add a polling timeout of its own.

---

## `secrets_request_rotation`

Same semantics as `request_provision { mode: "rotation" }`, but without ambiguity at the call site. The tool passes `mode = Rotation` to the registry; the UI shows a dialog with an explicit destructive-confirm checkbox ("I understand this overwrites the current secret").

### Request

```json
{
  "name": "secrets_request_rotation",
  "arguments": { "path": "team/<provider>/api-key" }
}
```

### Response

```json
{ "request_id": "prov-7c3e5a2d8b9f" }
```

Lifecycle is identical to `secrets_request_provision`. Use `secrets_poll_status` to watch progress.

---

## `secrets_propose_metadata`

Ask the user to apply edits to the metadata of an existing path. An edit-metadata dialog opens with a **diff preview**: current values on the left are read from the manifest (the trusted source), proposed values on the right come from the agent's payload.

### Request

```json
{
  "name": "secrets_propose_metadata",
  "arguments": {
    "path": "team/<provider>/api-key",
    "fields": {
      "description": "Updated description",
      "retrieval_url": "https://example.invalid/<provider>/new-url",
      "rotate_every_days": 60,
      "expires_at": "2027-01-01",
      "pattern_id": "<new-pattern-id>"
    }
  }
}
```

Fields in `fields` are all optional. Omitted fields are not proposed for change; they stay at their current value in the diff.

### Response

```json
{ "request_id": "prov-9a2b8c4f7e1d" }
```

### Trust boundary against prompt injection

This is a critical part of the protocol. The UI renders **only** the `current` column from the manifest — agent strings appear exclusively in `proposed`. That means:

- The agent cannot "rewrite" metadata via cleverly composed values that pretend to be existing.
- The user sees exactly what is being proposed.
- Any attempt to "swap in" a path through a description with special characters fails because the path itself is rendered from the `manifest`, not from the payload.

For details, see the "Trust boundary" section in `crates/devboy-mcp/src/secrets_provision.rs`.

---

## `secrets_propose_new_path`

Suggest **registering a new path** in the project manifest. The UI opens a discovery-style dialog with `suggested_path` as an editable starting point.

### Request

```json
{
  "name": "secrets_propose_new_path",
  "arguments": {
    "suggested_path": "team/<provider>/<new-purpose>",
    "metadata": {
      "description": "Newly discovered credential",
      "pattern_id": "<pattern-id>",
      "retrieval_url": "https://example.invalid/<provider>/tokens",
      "rotate_every_days": 90,
      "expires_at": null
    }
  }
}
```

### Response

```json
{ "request_id": "prov-2e4f6a8c1b3d" }
```

Unlike `propose_metadata`, the path is editable — the user can decline the agent's suggestion and pick a name of their own. The final decision stays with the human.

---

## `secrets_poll_status`

Status check for any `request_id` from any `request_*` or `propose_*` tool. One endpoint — uniform lifecycle semantics.

### Request

```json
{
  "name": "secrets_poll_status",
  "arguments": { "request_id": "prov-1f9b3a2c5e7d" }
}
```

### Response

```json
{
  "request_id": "prov-1f9b3a2c5e7d",
  "path": "team/<provider>/api-key",
  "kind": "provision",
  "status": { "kind": "ok" },
  "age_seconds": 27
}
```

Fields:

- `request_id` — echo of the request.
- `path` — the path the dialog originally opened on. Echoed for confirmation.
- `kind` — `provision` / `rotation` / `metadata-proposal` / `new-path-proposal`.
- `status.kind` — one of:
  - `pending` — dialog is open, the user has not hit Save or Cancel yet.
  - `ok` — user saved the value / accepted the proposal.
  - `cancelled` — user closed the dialog without saving.
  - `expired` — the 5-minute TTL elapsed; the registry marked the entry as Expired.
  - `failed { reason }` — the dialog failed to open (no launcher, daemon down) or the provider rejected the submission.
- `age_seconds` — how long ago the request was created.

If the `request_id` does not exist (was swept), the response is an error: `unknown request_id: <id>`.

### Polling pattern

```text
loop:
  resp = secrets_poll_status(request_id)
  if resp.status.kind == "pending":
    sleep(2..5 seconds)
    continue
  break

handle resp.status.kind:
  ok        → proceed (e.g. retry the high-level tool that needed this secret)
  cancelled → ask the user "what now?" — retry, abandon, etc.
  expired   → tell the user the dialog timed out; offer a fresh request
  failed    → show resp.status.reason; suggest devboy secrets agent start
```

Do not poll faster than once every 2 seconds — the dialog is no faster than a human types. A tight loop heats up CPU and telemetry without speeding anything up.

---

## End-to-end scenario: provision + retry

```text
User: "Use Jira to fetch the latest issues."

Agent: get_issues({"limit": 10})
       → ProviderError: missing token at team/<provider>/api-key

Agent thinks: secret missing, request provision.
Agent: secrets_describe({"path": "team/<provider>/api-key"})
       → { ... description, retrieval_url, capabilities_hint: "read", ... }

Agent: secrets_request_provision({"path": "team/<provider>/api-key"})
       → { "request_id": "prov-1f9b3a2c5e7d" }

Agent → User: "I've opened a dialog for the missing token. Paste the value
              from <retrieval_url>, then click Save."

# Agent polls every ~3 seconds
Agent: secrets_poll_status({"request_id": "prov-1f9b3a2c5e7d"})
       → { ..., "status": { "kind": "pending" }, "age_seconds": 12 }
... (repeat) ...
Agent: secrets_poll_status(...)
       → { ..., "status": { "kind": "ok" }, "age_seconds": 47 }

Agent → User: "Got it, retrying."
Agent: get_issues({"limit": 10})
       → 10 issues returned
```

Note: the agent **never** asks for the token's value. It only learns that a save happened; the token went from the dialog into the daemon, from the daemon into a proxy alias, and `get_issues` picked it up through the same proxy without the agent's involvement.

---

## What is **not** on the agent surface

These tools **do not exist** and will not appear:

- ❌ `secrets_get` / `secret.get` — handing the value to the agent. Replacement: a high-level provider tool with a proxy alias.
- ❌ `secrets_set` / `secret.put` — writing directly from the agent's payload. Replacement: `secrets_request_provision` → user types it themselves.
- ❌ `secrets_export` / dump — bulk export. Not in the architecture.

If a "convenience" tool seems like a shortcut, look again. The convenience of "passing the token directly" violates the invariant; the framework will not offer a workaround.

## Operational tools that are NOT in the agent surface

For DevOps / setup scenarios where convenience outweighs agent-safety, separate CLI commands exist (locally only — never via MCP):

- `devboy secrets validate` — manifest format check + live source probe.
- `devboy secrets rotate <path>` — interactive rotation for a developer (P13.1).
- `devboy secrets ui` — open the TUI/GUI inventory for manual review/edits (P12.2).
- `devboy secrets catalog list` / `validate` — token-catalog inspection (P20.9 / P20.10).

These commands require a human at the terminal and are not callable through JSON-RPC. The agent can *recommend* the user run them, but cannot run them itself.

---

## Protocol versioning

`PROTOCOL_VERSION = "1.0"` (see `crates/devboy-storage/src/plugin_protocol.rs`). Semantic breakdown:

- **Major bump (2.0)** — reply fields removed/renamed, `kind` enum semantics not back-compatible. Old agents get an error trying to use the new server.
- **Minor bump (1.x)** — new fields added (always optional), new status variants (the agent should treat unknown `kind` as `failed`), new tools in the `secrets_*` family.
- **Patch (1.x.y)** — bugfix, no wire-format change.

Recommendation for agent code: parse responses tolerantly — ignore unknown fields, interpret unknown `status.kind` as `failed`. That gives compatibility with future minor versions without an agent update.

---

## See also

- [`onboarding.md`](./onboarding.md) — manifest + router setup, without which `secrets_list` returns nothing.
- [`local-vault.md`](./local-vault.md) — where values physically live after a save from the dialog.
- [`token-catalog.md`](./token-catalog.md) — the JSON catalog that drives the dialog's "where to take from" / "how to validate" content.
- [`catalog-url-sources.md`](./catalog-url-sources.md) — serving the catalog over the network with sha-pinning + audit log.
- [`source-plugin-protocol.md`](./source-plugin-protocol.md) — how to add your own source that the agent tools can see.
- ADR-023 §3.7 — formal spec of the trust boundary and invariants.
- `crates/devboy-mcp/src/secrets_tool.rs` + `secrets_provision.rs` — implementation source.
