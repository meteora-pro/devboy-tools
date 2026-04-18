---
name: devboy-retro
description: Aggregate the last N days of traces, MRs, and CI runs to surface patterns worth fixing.
category: self-feedback
version: 1
compatibility: devboy-tools >= 0.18
activation:
  - "weekly retro"
  - "what patterns are there"
  - "where should I improve"
tools:
  - trace
  - get_merge_requests
  - get_merge_request_discussions
  - get_pipeline
  - get_job_logs
  - get_issues
---

# devboy-retro

Looks back over the last N days of session traces, recent merge
requests, and CI pipelines to surface recurring patterns: skills whose
success rate slipped, review feedback that keeps coming back, flaky
jobs. The output is a user-facing report with suggestions — the skill
never files tickets, never edits other skills, and never opens MRs.

## When to use

- At a weekly or bi-weekly retro, to anchor the conversation in
  evidence instead of anecdote.
- When a skill seems unreliable and you want to quantify the failure
  shape before rewriting it.
- Before an upgrade, to see which tool calls are most at risk if
  something changes.

## Procedure

### 1. Pick the window

- `--days 7` (default). Collect traces from the last N calendar days
  under `<scope>/.devboy/sessions/<YYYY-MM-DD>/` where `<scope>` is
  either the repo root (default) or `~/.devboy/` when `--global` is
  passed.
- If fewer than two days of data exist, warn on stderr — retros over
  a tiny window are noisy.

### 2. Begin the trace

```bash
result=$(devboy trace begin --skill devboy-retro)
SESSION_DIR=$(echo "$result" | jq -r .session_dir)
SESSION_ID=$(echo "$result" | jq -r .session_id)
```

Emit a `decision` event recording the window and the scope.

### 3. Aggregate per-skill session stats

Walk every `<date>/<skill>/<session_id>/meta.json` in the window —
each session lives in its own subdirectory, so a skill that ran four
times on one day produces four `meta.json` files under that day's
`<skill>/` folder. Per skill, aggregate across every completed
session in the window: total runs, success / failure / aborted
counts, total `tool_calls`, total `errors`, total duration, average
duration per run, and the most common `summary` strings for failing
runs. Skip any session whose `meta.json` has not yet recorded
`ended_at` / `outcome` — that run is still in flight and should not
skew the numbers.

Additionally, read each failing session's `trace.jsonl` to find
retry loops — sequences of `verify` events with `ok: false` followed
by more `tool_call` attempts. A skill with many retry loops is a
skill that could benefit from a stronger precondition check; record
the ratio `retried / total_failures` per skill.

Emit one `note` event per skill containing the aggregate numbers so
future retros have a stable trail.

### 4. Cross-reference CI history

Pull the recent merged MRs with a single list call, then iterate the
filtered result client-side — `get_merge_requests` returns a list, not
one MR at a time:

```bash
devboy tools call get_merge_requests '{"state":"merged","limit":100}'
```

Filter the returned list to merge timestamps inside the window, then
for the first ~20 entries call `get_pipeline` per MR:

```bash
devboy tools call get_pipeline \
  '{"mrKey":"mr#482","includeFailedLogs":true}'
```

Collect failing-job frequency keyed by job name. For the top three
failing jobs, call `get_job_logs` in search mode to pull the most
common error signature:

```bash
devboy tools call get_job_logs \
  '{"jobId":"<id>","pattern":"error|fail|panic","context":2,"maxMatches":10}'
```

Keep only the error shapes that repeat across multiple runs — a
single broken job is signal for the developer, not a pattern.

### 5. Cross-reference review feedback

For the same merged MRs:

```bash
devboy tools call get_merge_request_discussions \
  '{"key":"mr#482","limit":50}'
```

Group the discussion bodies by naïve keyword bucket (type-safety,
error-handling, testing, naming, i18n, performance, security). Count
how often each bucket appears across MRs. The top three buckets go
into the report.

### 6. Produce the report

Markdown to stdout:

```markdown
# Retro — last 7 days

## Skills with degraded success rate
- devboy-solve-issue — 6/10 success (was 9/10 the previous week);
  60% of failures retry more than twice; top summary:
  "gitlab returned 429".

## Frequent review feedback
- testing (mentioned in 9 MRs)
- error-handling (mentioned in 5 MRs)
- type-safety (mentioned in 4 MRs)

## Flaky CI signal
- integration::auth — 5/20 runs failed with "connection refused"
- clippy — 3/20 runs failed with "-D warnings" on a single rule

## Suggestions
- Add a 429 back-off to the get_issues call inside devboy-solve-issue.
- Update devboy-review-mr's checklist to call out type-safety explicitly.
- Investigate integration::auth — likely a race on the test fixture.
```

Omit sections with no entries. Keep the report tight; two screens of
text at most.

### 7. End the trace

```bash
devboy trace end \
  --session-dir "$SESSION_DIR" --session-id "$SESSION_ID" \
  --skill devboy-retro \
  --outcome "$OUTCOME" \
  --summary "<N> sessions, <M> MRs, <K> jobs analysed"
```

## Success criteria

- The report is driven entirely by numbers pulled from traces, MR
  history, and pipeline data — no hand-waving.
- Every suggestion points at a concrete skill, job, or feedback
  bucket.
- The skill is idempotent: running it twice over the same window
  produces the same report (modulo clock drift in timestamps).

## Guardrails

- Never auto-create issues, never edit a `SKILL.md`, never post a
  comment. The suggestions are text for a human to read.
- Redacted trace payloads (`<redacted:credential>`, `<redacted:token-pattern>`)
  are treated as opaque. Count them, do not try to un-redact them.
- If `get_pipeline` or `get_merge_request_discussions` fails, note
  the degradation in the report ("CI section omitted — pipeline
  lookup failed") rather than pretending everything is fine.

## Non-goals

- Does not compare across projects or repositories.
- Does not score individual developers; retros look at skills and
  systems, not people.
- Does not overlap with `devboy-daily-report` — that is a single-day
  summary, this one is a multi-day pattern detector.
