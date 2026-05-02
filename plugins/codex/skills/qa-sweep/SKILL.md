---
name: qa-sweep
description: Fan out parallel QA sub-agents against the `devboy` CLI — each one hunts a specific class of regression (exit codes, stdout hygiene, error propagation, schema drift, …) and reports findings into a shared bug log.
category: self-feedback
version: 1
compatibility: devboy-tools >= 0.18
activation:
  - "run QA on the CLI"
  - "sweep devboy for regressions"
  - "find regressions in devboy"
  - "full QA pass"
  - "parallel QA agents"
tools:
  - trace
---

# devboy-qa-sweep

Run a batch of narrowly-scoped QA sub-agents against a built `devboy` binary, each chasing one class of regression, and merge their findings into a single bug log. The skill does **not** exercise third-party provider APIs by itself — it focuses on contract / hygiene / schema-drift regressions that the unit test suite does not catch (exit codes, stdout vs stderr separation, `ProviderUnsupported` fallbacks, schema ↔ executor mismatches, etc.).

Unlike `devboy-daily-report` (trace-driven retrospective) and `devboy-retro` (multi-day pattern detector), this skill is **execution-driven**: it spins up the CLI many times with crafted inputs, diffs observed behaviour against a documented contract, and records every mismatch.

## When to use

- Right before a release: "give me every regression you can find before we tag 1.N.0".
- After a large merge (epic, stacked series, rename / refactor) that touched many subcommands.
- After a CI matrix expansion, when you want a behavioural smoke pass beyond "unit tests green".
- On demand when a user reports a vague "CLI feels off" — this skill's output gives an evidence-based triage.

Not a substitute for:
- `cargo test` / `cargo clippy` / `cargo fmt --check` (the skill assumes those already passed).
- Provider-specific integration tests with real credentials (out of scope — each sub-agent is opaque to the configured provider set; it uses local fakes or treats live providers as optional).

## Inputs

The skill takes three optional inputs, all with sensible defaults:

- `--binary <path>` — which `devboy` to test. Default: `./target/release/devboy`, falling back to `$PATH`.
- `--bug-log <path>` — where the merged findings go. Default: `/tmp/devboy-qa/BUGS_FOUND.md`.
- `--classes <a,b,c,…>` — comma-separated list of bug classes to run (see the agent charters below). Default: all of them.

## Procedure

### 1. Sanity-check the binary

Before spinning up any sub-agents, the main agent confirms the binary is invokable and prints a meaningful `--version`:

```bash
DEVBOY="${DEVBOY:-./target/release/devboy}"
"$DEVBOY" --version || { echo "devboy binary at $DEVBOY is not runnable"; exit 1; }
```

Also snapshots the git SHA of the source tree — every bug-log entry references it so findings are attributable to a specific build.

### 2. Begin the meta-trace

The sweep itself is a traced session — retros care about how often a QA pass surfaces new findings and how long it takes:

```bash
result=$(devboy trace begin --skill devboy-qa-sweep)
SESSION_DIR=$(echo "$result" | jq -r .session_dir)
SESSION_ID=$(echo "$result" | jq -r .session_id)
```

Record a `decision` event listing which classes will run.

### 3. Fan out sub-agents — one per bug class

Each class gets its own sub-agent invocation (Claude Code: `Agent` tool; other runtimes: equivalent task / subprocess primitive). They run in **parallel** — the charters are independent and non-destructive. Each sub-agent gets:

- The path to the binary.
- A writable scratch directory (its own — never shared).
- The bug-log path (append-only, each agent writes its own section).
- Permission to use `$HOME`, `$TMPDIR`, `/tmp` — nothing that modifies a real project.

Launch all sub-agents in one fan-out; the main agent then waits for every report.

### 3.A  `exit-codes` — Shell scripting readiness

**Charter.** Every error path must return non-zero. Shell scripts cannot differentiate success from failure if the CLI always exits 0.

**Probe set (non-exhaustive):**

| Case | Invocation |
|------|------------|
| unknown skill | `devboy skills show bogus-skill` |
| unknown tool | `devboy tools call bogus_tool '{}'` |
| missing required arg | `devboy tools call` (no NAME) |
| malformed JSON arg | `devboy tools call get_issues 'not-json'` |
| unknown context | `devboy context use unknown` |
| unknown agent flag | `devboy skills install x --agent bogus --dry-run` |
| conflicting flags | `devboy skills install x --global --local --dry-run` |
| no args in install | `devboy skills install` |
| install missing skill | `devboy skills install nonexistent --dry-run` |
| remove missing skill `--strict` | `devboy skills remove nonexistent --global --strict --dry-run` |
| empty stdin for pipe | `echo '' \| devboy format-pipeline` |
| bad stdin JSON | `echo 'bad' \| devboy format-pipeline` |
| 401 from provider | `DEVBOY_GITHUB_TOKEN=bad devboy test github` |
| `tools/call` with `isError: true` | call any tool unsupported by the configured provider |

**Expected:** every row → exit code `!= 0`.

**Report shape:** per failing row — the invocation, the stderr, the observed exit code, and (if the agent can infer it) the call chain that dropped the error.

### 3.B  `stdout-hygiene` — Pipe-ability

**Charter.** Any subcommand whose output is meant to be parsed by a script must not mix tracing logs into stdout. The canonical test is `cmd | jq .` — if `jq` rejects the first line, hygiene is broken.

**Probe set:**

| Invocation | Expected |
|------------|----------|
| `devboy tools call get_issues '{"limit":1}'` | pure JSON on stdout |
| `devboy tools list` | tabular / JSON, no interleaved `INFO`/`WARN` |
| `devboy tools call get_issue '{"key":"<real>"}'` | pure JSON |
| `devboy format-pipeline` (with valid JSON stdin) | pure TOON / JSON output |
| `devboy proxy status --json` | pure JSON |
| `devboy mcp` (one `initialize` + one `tools/list`) | every line on stdout must parse as JSON-RPC |

**Report:** for each broken invocation, a diff of where the noise starts and the env / subscriber config that routes it to the wrong stream.

### 3.C  `error-propagation` — Upstream failures surface to the caller

**Charter.** When a provider returns a real error (`NotFound`, `InvalidData`, `Http`, `Unauthorized`), the caller must see **that** error — not a generic "no provider supports X" fallback from `should_try_next_provider`.

**Probe set:**

- `tools call get_pipeline '{"branch":"empty-branch"}'` — expect a concrete `NotFound(...)` message, not `"No provider supports 'get_pipeline'"`.
- `DEVBOY_GITHUB_TOKEN=ghp_bad tools call get_issue '{"key":"gh#1"}'` → `Unauthorized` text.
- `tools call get_merge_request_diffs '{"key":"pr#999999"}'` → concrete `NotFound`, not a provider-skipping fallback.
- `tools call get_structure_forest '{"structureId":1}'` against a GitHub-only setup → `ProviderUnsupported` IS acceptable here (the provider legitimately does not implement the tool).

**Report:** the invocation, the error text the user saw, and whether it matches the expected variant. False-positive `"No provider supports …"` messages are the highest-severity finding.

### 3.D  `schema-sync` — Tool schema vs executor params

**Charter.** For every tool in `devboy tools list`, the arg names the schema declares must be exactly the arg names the executor deserialises — camelCase vs snake_case mismatches silently drop parameters through `unwrap_or_default()` in the current code.

**Procedure:**

1. Run `devboy mcp` with an `initialize` + `tools/list` stdin sequence; capture every `inputSchema.properties.<name>`.
2. For each tool, craft a minimal payload that exercises each declared parameter.
3. Call the tool via `devboy tools call <name> '<payload>'` against a local fake provider (GitHub test repo is OK) — compare the response against what each param should have done.
4. Flag any parameter that was silently ignored (e.g. a `state` value that did not actually filter).

**Report:** per tool, the full declared schema vs the names the executor actually honours.

### 3.E  `help-accuracy` — `--help` vs reality

**Charter.** Every `--help` page should accurately describe what the subcommand does. `--global` is not "upgrade everything across every recorded target"; it is "target `~/.agents/skills/`".

**Procedure:**

- Enumerate every `devboy <subcommand> [--subsubcommand] --help`.
- Extract the verbs ("Creates", "Returns", "Fails if …").
- Run the command in a controlled temp dir and check each verb.
- For flag descriptions, run with and without the flag — the described behaviour should match the observed diff.

**Report:** per mismatch, the help text and the contradicting observation.

### 3.F  `config-resolution` — Where does `.devboy.toml` come from?

**Charter.** All subcommands should agree on which `.devboy.toml` is active. Current bugs: `config list`/`config path` look only in the global config, while `tools call`/`context list`/`test` walk up from cwd — and cwd discovery can walk into an unrelated parent project.

**Procedure:**

- Create three isolated dirs: `/tmp/a` (empty), `/tmp/a/b` (contains `.devboy.toml`), `/tmp/a/b/c` (empty, subdir of b).
- From each directory, run `config list`, `config path`, `tools call get_issues`, `context list`, `test github`, `doctor`.
- Record which `.devboy.toml` each subcommand resolved.

**Report:** the resolution matrix. Inconsistencies are bugs.

### 3.G  `credential-resolution` — env vars vs keychain

**Charter.** `DEVBOY_<PROVIDER>_TOKEN` env vars should be honoured by every subcommand that consults credentials (not just `test <provider>`). `devboy doctor` should report the same credential state that `tools call` actually uses.

**Procedure:**

- Set `DEVBOY_GITHUB_TOKEN=<valid>`; no keychain entry.
- Run `devboy test github` (expect PASS), `devboy tools call get_issues '{}'` (expect real data), `devboy doctor` (expect "GitHub token present").

**Report:** per subcommand, "honours env var" / "does not".

### 3.H  `skills-lifecycle` — Install idempotence

**Charter.** Install is idempotent; re-install of the same bytes is a no-op; re-install of changed bytes respects `--force`; manifest stays in sync with disk.

**Procedure (per target: `--global`, `--agent claude`, `--agent all`):**

1. Fresh install → expect `installed` outcomes + manifest present.
2. Re-install → expect `unchanged` outcomes; manifest unchanged.
3. Edit a file → re-install without `--force` → expect `skipped` (user-modified).
4. Re-install with `--force` → expect `forced`.
5. Remove → expect files gone; manifest entry gone.

Compare the manifest SHA256 against the shipped `history.json` at each step.

**Report:** any step that does not match the contract above.

### 4. Collect findings

Each sub-agent appends a section to the bug log in this format:

```markdown
## devboy-qa-sweep / <class-id> — <short title>

**Run:** <timestamp> — binary <path> @ <git sha>
**Status:** FOUND | CLEAN

### Findings

- **[SEVERITY]** *Component* — one-sentence summary
  - **Repro:** <commands>
  - **Expected:** <behaviour>
  - **Actual:** <behaviour>
  - **Hint:** <grep-level pointer into the codebase, if any>
```

Severity scale: `BLOCKER` (release gate), `CRITICAL`, `MAJOR`, `MINOR`, `COSMETIC`.

### 5. Merge + summarise

The main agent:

1. Reads each sub-agent's section.
2. Collapses duplicates (two agents occasionally flag the same root cause — merge into one entry with the broader repro list).
3. Writes a header block: total findings, per-severity counts, class-level summary.
4. Ends the meta-trace (`devboy trace end ... --outcome success --summary "<N> findings across <K> classes"`).

### 6. Return a quick-read summary

After the bug log is written, the main agent prints a short text to stdout:

```
QA sweep: 17 findings (2 blocker, 3 critical, 9 major, 3 minor)
 - exit-codes: 5 findings
 - stdout-hygiene: 2 findings (1 critical)
 - error-propagation: 2 findings (1 critical)
 - schema-sync: 7 findings (2 blocker)
 - help-accuracy: 1 finding
 - config-resolution: 0 findings ✓
 - credential-resolution: 1 finding
 - skills-lifecycle: 0 findings ✓

Full log: /tmp/devboy-qa/BUGS_FOUND.md
```

## Success criteria

- Every requested class ran; every one produced either a non-empty findings list or a `Status: CLEAN` marker. Silent skips are not acceptable.
- The bug log file is created / appended to atomically — concurrent sub-agents never clobber each other's section.
- Every finding has a deterministic repro (exact commands + env vars + cwd) so a second run on the same commit reproduces the same output.
- The summary line counts match the per-class section totals.
- The meta-trace has one `start`, one `decision`, one `note` per sub-agent (status + finding count), one `end`.

## Guardrails

- **No real writes** to third-party providers unless the operator explicitly passes `--allow-writes`. The default test repo for live provider calls must be scoped by env var (`DEVBOY_QA_TEST_REPO=owner/repo`) so a typo cannot spam a production project.
- **No secret values in the bug log.** Env vars holding tokens are captured by name only; their values are elided. The `trace` redactor already covers the meta-trace; sub-agents must mirror the same discipline on their own stdout / stderr captures.
- **Scratch dirs are cleaned up** between sub-agents — no cross-contamination of `HOME` / `XDG_CONFIG_HOME` / `.devboy.toml` search paths.
- **Stop on binary mismatch.** If any sub-agent detects it is testing a binary whose git SHA differs from the one the main agent captured in step 1, it exits immediately with `FOUND BLOCKER: binary swapped mid-sweep`.
- **Never delete the bug log.** Overwrites are allowed only with an explicit `--reset` flag.

## Non-goals

- **Not a unit-test replacement.** Individual `unwrap()` / off-by-one / rare deserialisation edge cases are the province of `cargo test`; this skill is for behavioural hygiene only.
- **Not a provider conformance suite.** Live provider edge cases (GitHub Projects v2 quirks, GitLab rate-limit headers, Jira ADF rendering) need dedicated integration tests — the sub-agents here use at most one provider happy path to anchor each class.
- **Not a performance benchmark.** `devboy benchmark` already covers the format pipeline; latency / throughput findings are out of scope.
- **Not a fix pipeline.** Sub-agents *report*; they never open issues, push branches, or edit source files. The user decides what to triage from the bug log.
