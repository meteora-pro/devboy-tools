---
name: devboy-run-and-verify
description: Run a command, trace every step, verify the expected outcome, and retry on failure.
category: self-feedback
version: 1
compatibility: devboy-tools >= 0.18
activation:
  - "run and verify"
  - "verify this command worked"
  - "retry until green"
tools:
  - trace
---

# devboy-run-and-verify

The opinionated wrapper that the other skills in this category assume.
It runs one command, emits a structured session trace for every attempt,
checks the expected outcome, and decides whether to retry. Downstream
skills (`devboy-daily-report`, `devboy-retro`,
`devboy-knowledge-extract`) consume the trace this skill produces.

## When to use

- You need to run a shell command whose success is not obvious from the
  exit code alone (e.g. "the build exited 0 but printed WARNING lines we
  want to catch").
- You want a trace that later skills can read to understand what
  happened and how long it took.
- You want one, predictable retry policy rather than hand-rolling
  retries inside each calling skill.

## Procedure

### 1. Parse the caller's intent

Collect the following arguments from the caller:

- `--command "<shell command>"` — the command to execute.
- An expected-outcome specification. One of:
  - `--expect-exit 0` — exact exit code.
  - `--expect-stdout "<substring>"` — stdout must contain this string.
  - `--expect-file "<path>"` — this path must exist after the run.
  - `--expect-check "<verification command>"` — a shell command whose
    exit code 0 means "the main command did what it was supposed to".
- `--max-retries 2` (default `2`). The command runs up to
  `1 + max-retries` times.
- `--skill-name <name>` — the name of the skill that invoked this
  wrapper. Traces are written under
  `.devboy/sessions/<YYYY-MM-DD>/<skill-name>/` so downstream readers
  attribute the work correctly.
- `--allow-destructive` — required before running anything that matches
  the destructive-command guardrail (see below).

If an expected-outcome flag is missing, the skill exits with an error —
there is nothing to verify against.

### 2. Begin the session

```bash
result=$(devboy trace begin --skill "$SKILL_NAME")
SESSION_DIR=$(echo "$result" | jq -r .session_dir)
SESSION_ID=$(echo "$result" | jq -r .session_id)
```

Emit a `decision` event that records what the caller asked for:

```bash
devboy trace event \
  --session-dir "$SESSION_DIR" --session-id "$SESSION_ID" \
  --skill "$SKILL_NAME" --phase decision \
  --payload "$(jq -nc --arg cmd "$COMMAND" --arg expect "$EXPECT" \
               '{question:"what to run",decision:$cmd,expected:$expect}')"
```

### 3. Per-attempt loop

For `attempt` in `1..=1+max_retries`:

1. Emit a `tool_call` event describing the command about to run and
   the attempt number.
2. Run the command with stdout + stderr captured to a temporary file.
   Record the start time, the end time, and the exit status.
3. Summarise the output: keep the first 20 lines, the last 20 lines,
   and every line matching `ERROR|WARN|FAIL|panic`. Everything else
   can be dropped — trace payloads should stay small (ADR-015 risks).
4. Emit a `tool_result` event with `{ok, exit, duration_ms, summary}`.
5. Run the expected-outcome check. Emit a `verify` event with
   `{check, ok, detail}`. The check is:
   - exit code comparison, or
   - substring search in the captured stdout, or
   - `test -e <path>`, or
   - the `--expect-check` shell command.
6. If the verify event is `ok: true`, break out of the loop.
7. Otherwise, if retries remain, emit a `note` event explaining why
   the next attempt is happening ("expect-exit 0 but got 1; retrying"),
   and continue. If retries are exhausted, fall through to step 4.

### 4. End the session

Pick the outcome:

- `success` — some attempt produced `ok: true` on both tool-result and
  verify.
- `failure` — every attempt failed the verification.
- `aborted` — the caller interrupted mid-run, or a destructive command
  was rejected by the guardrail.

```bash
devboy trace end \
  --session-dir "$SESSION_DIR" --session-id "$SESSION_ID" \
  --skill "$SKILL_NAME" --outcome "$OUTCOME" \
  --summary "$SHORT_SUMMARY"
```

### 5. Surface the result

Print a 3-to-5-line summary to stdout naming: the command, attempts
used, final outcome, and the path to the trace directory. Do not
reprint the full captured output — the caller can `cat` the trace if
they want it.

## Success criteria

- Exactly one `start` and one `end` event per invocation.
- Every attempt contributes a matching `tool_call` / `tool_result` /
  `verify` trio.
- `meta.json` lands with `outcome` set to one of `success | failure |
  aborted`.
- A failing run still produces a complete, well-formed trace.

## Guardrails

Destructive commands are refused unless the caller passes
`--allow-destructive`. The refusal path still opens a session, emits a
`note` event describing the rejection, and ends with
`outcome: aborted` so downstream skills can see what happened.

Treat the following substrings (case-insensitive) as destructive:

- `git push --force`, `git push -f`, `git reset --hard`,
  `git clean -fdx`, `git branch -D`
- `rm -rf`
- `drop table`, `drop database`, `truncate`
- `kubectl delete`, `helm uninstall`

## Non-goals

- This skill does not parse structured output to extract metrics; the
  trace summary is intentionally textual.
- It does not chain multiple commands. A pipeline of commands is
  multiple invocations of this skill, one per step.
- It does not post results anywhere. Notification belongs to
  `devboy-notify` (category 5).
