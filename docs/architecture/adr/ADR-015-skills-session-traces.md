---
id: ADR-015
title: Skills self-feedback loop — session trace format
status: proposed
date: 2026-04-17
deciders: ["Andrei Mazniak"]
tags: ["skills", "observability", "self-feedback"]
supersedes: null
superseded_by: null
---

# ADR-015: Skills session traces

## Status

**proposed** — design agreed; implementation pending together with ADR-012.

## Context

The self-feedback category of skills (ADR-012, category 03 — `devboy-retro`, `devboy-daily-report`, `devboy-knowledge-extract`, `devboy-run-and-verify`) is only useful if there is **something to look back at**. We need a simple, standard way for skills (and any external agent that wants to opt in) to emit a trace of what they did, so that downstream skills can summarise, critique, or learn from it.

Requirements:

1. **Cheap to write.** Skills should be able to append a line without setting up a framework.
2. **Cheap to read.** A retro skill should be able to `grep`, `jq`, `tail`, or read JSONL line-by-line without pulling in a parser.
3. **Self-contained per session.** Traces from one session must not bleed into another.
4. **Repo-scoped by default.** The trace lives with the project it describes; moving or cloning the repo moves the trace with it.
5. **Privacy-aware.** Never log secret values (env vars, tokens). Redact known-sensitive patterns.

## Decision

> **Decision:** Session traces are **append-only JSONL** files at `<target>/.devboy/sessions/<ISO-date>/<skill>/trace.jsonl`. Each line is one JSON object keyed by a small, stable set of fields. The format is skill-agnostic — any caller can write to it.

### Path layout

```
<target>/.devboy/sessions/
├── 2026-04-17/
│   ├── devboy-solve-issue/
│   │   ├── trace.jsonl           # append-only event stream
│   │   └── meta.json             # session-level metadata (start, end, skill, version, outcome)
│   ├── devboy-review-mr/
│   │   └── trace.jsonl
│   └── devboy-run-and-verify/
│       └── trace.jsonl
└── 2026-04-18/
    └── ...
```

- `<target>` is the repository root by default, or `~/.devboy/` when `--global` is set (symmetric with ADR-013).
- `.devboy/sessions/` is added to `.gitignore` by default — traces are operational output, not code. Users can opt in to committing them.
- One directory per date keeps the layout manageable; listing a day's work is one `ls`.
- One subdirectory per skill invocation. Long-running sessions reuse the same directory across events.

### Event schema — `trace.jsonl`

One JSON object per line. All fields are optional except `ts`, `phase`, and `skill` — extra fields are preserved verbatim.

```json
{
  "ts": "2026-04-17T12:34:56.789Z",
  "skill": "devboy-solve-issue",
  "session_id": "01J123ABC...",              // ULID per session — same across all events
  "phase": "tool_call",                        // see phase enum below
  "payload": {
    "tool": "get_issues",
    "args": { "state": "open", "limit": 20 },
    "duration_ms": 143,
    "ok": true,
    "result_summary": "20 issues, 3 with label=bug"
  }
}
```

### `phase` enum

| Phase | When to emit | Typical payload |
|-------|--------------|-----------------|
| `start` | First event of the session | `input` (user prompt / args), `cwd`, `devboy_version` |
| `decision` | Skill reasoning outcome | `question`, `decision`, `rationale` (human-readable) |
| `tool_call` | Before invoking a tool | `tool`, `args` |
| `tool_result` | After a tool returns | `tool`, `ok`, `duration_ms`, `result_summary` (truncated), `error` |
| `verify` | A check ran against produced output (tests, lint, dry-run) | `check`, `ok`, `output` |
| `artifact` | Non-trivial file produced | `path`, `kind`, `size_bytes` |
| `note` | Free-form human-readable log | `message` |
| `end` | Last event of the session | `outcome: success\|failure\|aborted`, `summary` |

Unknown phases must be silently ignored by readers.

### `meta.json` per session

A sibling file written at session end (or updated periodically during a long session):

```json
{
  "session_id": "01J123ABC...",
  "skill": "devboy-solve-issue",
  "skill_version": 3,
  "devboy_version": "0.18.0",
  "started_at": "2026-04-17T12:34:56.789Z",
  "ended_at": "2026-04-17T12:41:03.221Z",
  "outcome": "success",
  "input_summary": "solve DEV-123",
  "tool_calls": 7,
  "errors": 0
}
```

This gives retro skills a quick index without having to scan the full trace.

### Privacy — redaction

A small, deterministic redactor runs before every write:

- **Hard strip:** any value matching an entry in the current `CredentialStore` (resolved at redaction time) is replaced with `"<redacted:credential>"`. Same for env vars with names matching `*_TOKEN`, `*_SECRET`, `*_KEY`, `*_PASSWORD`, `*_PASSPHRASE`, `AUTHORIZATION`, `COOKIE`.
- **Soft strip:** tokens that match common provider prefixes (`ghp_`, `glpat-`, `pk_`, `sk-`, `xoxb-`, `Bearer `) are replaced with `"<redacted:token-pattern>"` even if they do not match the resolved credential (e.g. a token pasted into an error message).
- **Opt-out:** the user can set `DEVBOY_TRACE_REDACTION=off` to disable redaction for local debugging. Never default to off.

The redactor runs on the full payload tree, including nested objects. It does not attempt DLP-grade detection — its job is to stop accidental leakage of the obvious things.

### Retention

We **do not** auto-prune traces. They are user-owned data and small enough that retention is not an immediate concern. A future `devboy sessions prune` subcommand can add policy-based cleanup; for now users who want to rotate can `rm -rf .devboy/sessions/2026-*`.

### Writer API (inside `devboy-skills`)

```rust
// crates/devboy-skills/src/trace.rs
pub struct SessionTracer { /* ... */ }

impl SessionTracer {
    pub fn begin(skill: &str, target: TraceTarget) -> Result<Self>;
    pub fn event(&self, phase: Phase, payload: serde_json::Value) -> Result<()>;
    pub fn end(self, outcome: Outcome, summary: &str) -> Result<()>;
}
```

Internal use. The real integration story is the CLI-level helper:

```
devboy trace begin --skill <name>                          # prints session_id + trace path
devboy trace event --phase tool_call --payload '{...}'     # append one event
devboy trace end --outcome success --summary "..."         # finalise meta.json
```

Skills invoke these `devboy trace` subcommands from their shell-based recipes; the `SessionTracer` type is only needed by Rust-native callers.

## Consequences

### Positive

- ✅ Retro skills have a simple, structured artefact to read — JSONL over many standard tools
- ✅ Any external tool or agent can write into the same format — the protocol is the file layout, not a Rust trait
- ✅ Repo-scoped default means traces move with the project
- ✅ Redaction is applied centrally — individual skills don't have to remember what is a secret
- ✅ Phase enum keeps events interpretable without forcing a schema rigour that would block skill authoring

### Negative

- ❌ The traces can accumulate on disk. No automatic cleanup yet — users who run many sessions a day will eventually want `devboy sessions prune`.
- ❌ Redaction is best-effort. A creative skill could still leak a secret via custom encoding; the user has to trust the skills they run.

### Risks

- ⚠️ **Trace explosion during long sessions.** Skills that run in a loop could write tens of thousands of events. Mitigation: keep payload summaries short, drop raw bodies for routine tool calls, point at artefacts (files) rather than inlining them.
- ⚠️ **Inconsistent writers.** A community skill that writes malformed JSON would trip up readers. Mitigation: JSONL parsers in the reader skills skip malformed lines and emit a warning rather than aborting.

## Alternatives Considered

### Alternative 1: SQLite database of events

**Description:** `~/.devboy/sessions.db` with structured tables (events, sessions, tools).

**Why not chosen:** Harder to inspect with `cat`/`jq`/`tail`; requires a library; concurrent writes need locking. JSONL is the smallest thing that gives us append-only, structured-enough events.

### Alternative 2: Structured logs through `tracing` crate

**Description:** Use the existing `tracing` subscriber to emit events to a JSON file.

**Why not chosen:** Couples the trace format to the logging stack. Skills that are shell scripts would have to do extra work to integrate. The `devboy trace` CLI keeps the writer side symmetric between Rust and shell callers.

### Alternative 3: Opaque binary format

**Description:** Protobuf or MessagePack per event for denser storage.

**Why not chosen:** Optimising for a non-problem. Events are small; disk is cheap; readability is the priority.

### Alternative 4: Global-only traces

**Description:** Always write to `~/.devboy/sessions/`, regardless of project.

**Why not chosen:** Breaks the "skills move with the repo" story and mixes traces from different projects. Repo-local is a better default; `--global` is available when the user explicitly wants merged history.

## Implementation

- **Writer:** `crates/devboy-skills/src/trace.rs` (Rust API) + `crates/devboy-cli/src/main.rs` (CLI subcommands)
- **Redactor:** small module under `trace::redact` that walks serde_json::Value trees
- **Schema validators:** unit tests ensuring `trace.jsonl` round-trips through serde and that unknown phases are tolerated

Related issues: see ADR-012.

## References

- [ADR-012: Skills subsystem](./ADR-012-skills-subsystem.md)
- [ADR-013: Skills install targets](./ADR-013-skills-install-targets.md)
- [JSON Lines](https://jsonlines.org/)

---

## Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-04-17 | Andrei Mazniak | Initial version |
