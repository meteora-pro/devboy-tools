---
name: devboy-repair
description: Diagnose and fix a broken devboy-tools setup — corrupt config, missing tokens, keychain trouble, wrong paths.
category: self-bootstrap
version: 1
compatibility: devboy-tools >= 0.18
activation:
  - "repair devboy"
  - "fix devboy"
  - "devboy is broken"
  - "devboy doctor failing"
tools:
  - doctor
  - config
  - test
---

# devboy-repair

Walk a misbehaving `devboy-tools` setup back to health. This skill is driven by `devboy doctor --format json` — the structured output is the source of truth for what's wrong, and every repair step maps to a diagnostic code.

## When to use

- `devboy doctor` exits non-zero.
- Tool calls return `ProviderUnsupported` unexpectedly.
- The user reports "it worked yesterday, now it does not".
- `devboy test <provider>` prints a 401 / 403 / network error.

If the issue is "nothing is configured yet" — use `devboy-setup` instead. This skill assumes a prior configuration existed.

## Procedure

### 1. Pin the fault

```bash
devboy doctor --format json > /tmp/devboy-doctor.json
jq '.' /tmp/devboy-doctor.json
```

Read the JSON: every check has `{ id, status, message, remediation }`. `status = "fail"` entries are the ones to fix.

If the command itself fails to run, `devboy` is not on `PATH` — install or re-link the binary before continuing.

### 2. Classify by check id

Work through the failing checks in order. Common buckets:

**`config.*`** — the `.devboy.toml` is missing, malformed, or points at something that no longer exists.

- `config.exists` fails → run `devboy init` (see `devboy-setup`).
- `config.valid_toml` fails → `jq .` or `cat .devboy.toml` to find the syntax error; back up and re-run `devboy init --force` if unsalvageable.
- `config.contexts.<name>.missing` → edit `.devboy.toml` to either remove the stale context reference or re-run `devboy init` to regenerate.

**`providers.*`** — credentials are missing or invalid.

- `providers.<name>.no_token` → `devboy config set-secret <name>.token` (or set the env var on CI).
- `providers.<name>.unauthorised` (401) → the token is wrong or expired. Rotate and re-store.
- `providers.<name>.forbidden` (403) → the token lacks the required scopes. Re-issue the token with the scopes listed in the remediation hint.
- `providers.<name>.unreachable` → network or DNS issue. Verify with `curl -v <base-url>`.

**`keychain.*`** — the OS keychain is not reachable.

- macOS / Windows keychain locked → unlock the user session; re-run.
- Linux headless (no D-Bus) → move to env vars: `export DEVBOY_<PROVIDER>_TOKEN=...` and re-run `devboy doctor`.

**`proxy.servers.*`** — an upstream MCP proxy is not responding.

- Network failure → verify with `curl -v <proxy URL>`.
- Bad token → re-issue via `devboy proxy add <name> --url <url> --force` with a new `--token`.

**`remote_config.*`** — the remote config endpoint is down or the token is wrong.

- 401 / 403 → re-issue the `--remote-config-token`.
- 5xx / timeout → retry; if persistent, the remote endpoint is the problem and local config takes over (remote config is best-effort).

### 3. Re-verify

After each fix:

```bash
devboy doctor --format json | jq '[.checks[] | select(.status=="fail")] | length'
```

Zero failing checks is the target. Repeat step 2 until the number reaches zero.

### 4. Smoke-test the tool bundle

```bash
devboy tools list
devboy tools call get_issues '{"limit": 3}'
```

Either must produce real data. `ProviderUnsupported` at this point means the provider is mis-configured (wrong project id, wrong list id, wrong repo owner) — go back to step 2.

## Guardrails

- **Never print token values** into the chat. When the user asks "what is my token?" the answer is "it lives in your keychain — re-issue it from the provider if you need a copy". Treat every `*_token` / `set-secret` argument as opaque.
- **Do not commit changes to `.devboy.toml`** automatically — config changes are a user decision.
- **If two checks disagree**, trust `devboy doctor` — it is the only deterministic source of ground truth here.

## Success criteria

- `devboy doctor` exits zero with no failing checks.
- At least one real tool call against each previously-broken provider succeeds.
- If the session started with a specific complaint from the user (e.g. "get_issues returns nothing"), the exact reported behaviour is now correct.
