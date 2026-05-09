---
name: daily-report
description: Summarise today's trace activity, merged MRs, and closed issues into a single report.
category: self-feedback
version: 1
compatibility: devboy-tools >= 0.18
activation:
  - "daily report"
  - "what did I do today"
  - "summarise today"
tools:
  - trace
  - get_merge_requests
  - get_issues
---

# daily-report

Reads today's session traces and cross-references them with the git
provider's recent activity to produce a short Markdown report. The
output goes to stdout — nothing is posted anywhere. Users who want the
report delivered to a chat pipe it into `notify` (category 5).

## When to use

- At the end of a working day, to answer "what did I actually do
  today?".
- Before a standup, to compile a from-the-trace view of yesterday's
  activity (run with `--date 2026-04-16`).
- Any time an auditor wants a reproducible summary of session outcomes
  against real git-provider events.

## Procedure

### 1. Resolve the sessions directory for the day

Pick the date (default: today in the local timezone) and the scope:

- Repo-local (default): `<repo>/.devboy/sessions/<YYYY-MM-DD>/`.
- Global (`--global`): `~/.devboy/sessions/<YYYY-MM-DD>/`.

If neither directory exists, exit with an error that tells the user
which path was checked.

### 2. Begin a trace for this report

```bash
result=$(devboy trace begin --skill daily-report)
SESSION_DIR=$(echo "$result" | jq -r .session_dir)
SESSION_ID=$(echo "$result" | jq -r .session_id)
```

This skill is itself traceable — retro runs will see that a daily
report ran, how long it took, and whether it succeeded.

### 3. Walk the per-session subdirectories

The trace subsystem writes each session under
`<YYYY-MM-DD>/<skill>/<session_id>/`, so a single skill can produce
several sibling session directories on the same day. Walk every
`<skill>/<session_id>/` pair under the day:

1. If `meta.json` is missing, skip the directory — the session is
   still in flight. Record a single `note` event listing the skipped
   sessions so they show up in the report as "in progress".
2. Otherwise, read `meta.json` and pull:
   - `skill`, `outcome`, `tool_calls`, `errors`,
   - `summary`, `started_at`, `ended_at`.
3. Aggregate per-skill counts across **all** that skill's session
   directories: total runs, success / failure / aborted, total
   tool-calls, total errors, average duration.

Emit an `artifact` event per session directory so the retro skill
can later find the raw data:

```bash
devboy trace event ... --phase artifact \
  --payload "$(jq -nc --arg path "$SESSION_DIR" '{path:$path,kind:"session-dir"}')"
```

### 4. Cross-reference with the provider

Two tool calls, each wrapped in a `tool_call` / `tool_result` pair:

```bash
devboy tools call get_merge_requests \
  '{"state":"merged","limit":50}'
devboy tools call get_issues \
  '{"state":"closed","limit":50}'
```

Filter the returned lists down to items whose merge / close timestamp
falls on the report's date. Neither `get_merge_requests` nor
`get_issues` has a first-class `since` parameter in the current
schema — do the date filter in the skill, not on the provider side.

Record `ok:false` on the `tool_result` if the call fails, but keep
going; a daily report is useful even if the tracker is offline.

### 5. Assemble the Markdown report

Structure:

```markdown
# Daily report — <YYYY-MM-DD>

## Sessions
- run-and-verify — 8 runs (7 success, 1 failure, avg 4.2s)
- solve-issue — 2 runs (2 success, avg 2m31s)
...

## Merged MRs
- mr#482 — "fix(auth): refresh token rotation"
- mr#485 — "refactor(api): split user service"

## Closed issues
- DEV-411 — "Cannot invite user with plus-sign email"

## Notable failures
- run-and-verify 14:02 — "cargo test" failed on integration::auth
```

Skip any section that has no entries. Keep the report short — a
hundred lines at most.

### 6. End the trace

```bash
devboy trace end \
  --session-dir "$SESSION_DIR" --session-id "$SESSION_ID" \
  --skill daily-report \
  --outcome "$OUTCOME" \
  --summary "<N> sessions, <M> MRs merged, <K> issues closed"
```

Print the assembled Markdown to stdout and exit.

## Success criteria

- The report lists one line per skill that ran today, with accurate
  counts drawn from `meta.json`.
- In-flight sessions (no `meta.json`) are listed under "in progress"
  rather than silently dropped.
- MRs and issues with a date-stamp outside the target day are not
  shown.
- The skill never mutates anything — no comments posted, no issues
  touched.

## Guardrails

- Redacted trace payloads stay opaque. If a trace line contains
  `<redacted:...>`, carry it into the report verbatim; do not try to
  reconstruct the original value.
- If both the repo-local and `--global` trace directories are absent,
  exit with a non-zero status rather than producing an empty report.

## Non-goals

- This skill does not post the report. Use `notify` from
  category 5 if delivery is needed.
- It does not create issues, update tickets, or touch git in any way.
- It does not analyse long-term trends — that is `retro`.
