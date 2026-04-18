---
name: devboy-knowledge-extract
description: Extract the lesson from a session that failed, then succeeded, and propose where to codify it.
category: self-feedback
version: 1
compatibility: devboy-tools >= 0.18
activation:
  - "extract the lesson"
  - "what did we learn"
  - "codify this fix"
tools:
  - trace
---

# devboy-knowledge-extract

Inspects a single session trace that went from a failure streak to a
clean success, pulls out what changed between "not working" and
"working", and proposes where the lesson belongs — a SKILL.md edit, an
`AGENTS.md` / `CLAUDE.md` bullet, or a ticket. **The skill is
read-only.** It prints a proposal to stdout; the user decides whether
to act on it.

## When to use

- Right after a painful debug that eventually passed — the shape of
  what worked is still fresh, but will fade within a day.
- On a session that `devboy-daily-report` flagged as "recovered after
  multiple failures".
- When reviewing a teammate's trace to understand how they unblocked
  themselves.

## Procedure

### 1. Locate the session

Accept one of:

- `--session-dir <path>` — absolute or relative path to a
  `.devboy/sessions/<YYYY-MM-DD>/<skill>/<session_id>/` directory. If
  the directory contains `meta.json` and `trace.jsonl`, use it
  directly.
- `--pick` — interactive mode: enumerate today's per-session
  directories (`.devboy/sessions/<YYYY-MM-DD>/<skill>/<session_id>/`)
  whose `meta.json` reports `outcome = success` and `errors > 0`,
  and let the user pick one. This is the common follow-up after
  `devboy-daily-report`.

Exit with a clear error if neither flag is set or the directory is
not a well-formed session.

### 2. Begin the meta-trace

The extract itself is traced — retros care about how often the team
reaches for this skill.

```bash
result=$(devboy trace begin --skill devboy-knowledge-extract)
SESSION_DIR=$(echo "$result" | jq -r .session_dir)
SESSION_ID=$(echo "$result" | jq -r .session_id)
```

Record a `decision` event naming the target session dir.

### 3. Parse the target trace

Read `trace.jsonl` line by line. Lines that fail to deserialise as
JSON are skipped with a single `note` event — do not abort. For each
valid record keep: `ts`, `phase`, `payload`.

Compute three spans:

- **Failure streak.** A contiguous run of events where either
  `tool_result.ok = false` or `verify.ok = false` — possibly
  interleaved with `note`, `decision`, or further `tool_call` events
  that target the same tool or check.
- **Flip point.** The first event after the streak where
  `tool_result.ok = true` or `verify.ok = true` on the same tool or
  check as the streak.
- **Setup delta.** The `decision` and `note` events that appear
  between the last failing event and the flip point — those record
  what the agent or user tried differently.

If the trace has no failure streak (only clean successes), print a
short message saying there is nothing to extract and end the meta-
trace with `outcome: aborted`.

### 4. Draft the lesson

One short paragraph, plain English. Template:

> After failing `<tool-or-check>` `<N>` times with `<common-error>`,
> switching to `<approach-described-in-setup-delta>` fixed it
> because `<why, if the decision/note events state it>`.

If the trace does not justify the "because" clause, leave it out
rather than making something up.

### 5. Suggest where to codify the lesson

Classify the fix and propose exactly one home:

- **Tool invocation pattern** (e.g. "call `get_pipeline` with
  `includeFailedLogs: false` when the MR is large") — propose a
  SKILL.md edit on the skill that owns the call. Name the file
  (`skills/<category>/<skill>/SKILL.md`) and quote the exact
  sentence to add.
- **Project convention** (e.g. "this repo requires migrations in
  snake_case") — propose a bullet for `CLAUDE.md`, `AGENTS.md`, or
  the project's own guide, quoting the sentence.
- **Systemic infra bug** (e.g. "CI cache corrupts itself on
  concurrent merges") — propose a ticket via
  `devboy tools call create_issue` (draft only), including a draft
  title and two-sentence reproduction, but do not create it.

Only one home per lesson. If the fix really belongs in two places,
split the extraction into two successive invocations.

### 6. Emit the proposal

Print to stdout:

```markdown
# Lesson extracted from <session-dir>

## What happened
<one paragraph>

## Lesson
<one paragraph>

## Proposed codification
Target: skills/02-code-review/devboy-review-mr/SKILL.md
Edit:
> Add the following to the "Procedure" section, after step 3:
> "When the MR touches more than 50 files, pass
>  includeFailedLogs: false to get_pipeline — otherwise the response
>  overflows the context window."
```

Do **not** apply the edit. Do **not** open the file. Do **not** create
the ticket.

### 7. End the meta-trace

```bash
devboy trace end \
  --session-dir "$SESSION_DIR" --session-id "$SESSION_ID" \
  --skill devboy-knowledge-extract \
  --outcome "$OUTCOME" \
  --summary "lesson proposal for <original-skill>"
```

## Success criteria

- Every proposal cites a specific file path or a concrete ticket
  target — no "maybe add a note somewhere".
- The "What happened" paragraph points at real `tool_call` /
  `tool_result` / `verify` events with timestamps from the trace.
- Running the skill twice on the same session produces the same
  proposal.

## Guardrails

- **Read-only.** The skill never writes into another skill's
  `SKILL.md`, never opens a PR, never calls `create_issue` or
  `add_issue_comment`. Proposals are text on stdout.
- Redacted trace fields are opaque. If the failure streak's error
  message was redacted, quote it verbatim (`<redacted:token-pattern>
  in response body`). Do not guess at the underlying value.
- If the trace is malformed or the session has no failure-to-success
  transition, say so and end with `outcome: aborted`.

## Non-goals

- Does not build a knowledge base or index of past lessons.
- Does not rank lessons by importance.
- Does not replace a human reviewer — the user still decides whether
  the proposed edit is worth making.
