---
name: meeting-to-tasks
description: Extract deduplicated action items from a meeting, confirm with the user, then create issues in the configured tracker.
category: meeting-notes
version: 1
compatibility: devboy-tools >= 0.18
activation:
  - "turn this meeting into tickets"
  - "create tasks from the meeting"
  - "extract action items"
  - "file tickets from the call"
  - "make issues from the transcript"
tools:
  - get_meeting_notes
  - get_meeting_transcript
  - get_issues
  - create_issue
  - link_issues
---

# meeting-to-tasks

Convert a meeting into concrete, trackable work. The skill is deliberately conservative: extract → deduplicate → **confirm with the user** → create. Automatic creation from a transcript produces nonsense tickets far too often — human review before the first `create_issue` is not optional.

## When to use

- The user references a meeting that produced to-dos and wants them in the tracker.
- A standup, planning, or retro transcript exists and the commitments need a paper trail.
- Follow-up from `devboy-meeting-search` when the user says "file tickets for this one".

## Procedure

### 1. Pin the meeting

The skill operates on one meeting at a time. If the id is not known, fetch the short metadata list and confirm with the user:

```bash
devboy tools call get_meeting_notes '{
  "from_date": "<window start ISO>",
  "to_date":   "<window end ISO>",
  "limit": 10
}'
```

### 2. Prefer the provider's action items

`get_meeting_notes` returns an `action_items` array populated by the provider's summariser. When it is non-empty, use it — it is already filtered to imperative commitments:

```bash
devboy tools call get_meeting_notes '{
  "from_date": "<narrow window around the meeting>",
  "to_date":   "<+1 day>",
  "limit": 5
}'
```

Take the `action_items` field from the matching meeting. This is cheaper and more reliable than parsing a transcript.

### 3. Fallback: parse the transcript

If `action_items` is empty or missing, fetch the transcript and extract candidates manually. `devboy tools call` has no `--budget` flag — the format-pipeline trims the response to fit the configured tool budget internally, so just call the tool and the runtime handles truncation:

```bash
devboy tools call get_meeting_transcript '{"meeting_id": "<id>"}'
```

Look for:

- Imperative verb phrases attributed to a specific participant ("I'll draft the RFC", "Alice will open a ticket about X").
- "Let's…" / "We need to…" statements where someone assented.

Skip hypotheticals, questions, and vague intentions ("we should probably look into that" with no commitment attached).

### 4. Deduplicate ruthlessly

A 45-minute call will restate the same commitment three or four times. Before creating anything, collapse the list:

- Normalise the verb + object (e.g. "update the docs", "fix docs", "I'll update the documentation" → one item).
- Merge paraphrases of the same commitment made by the same person.
- Keep distinct items when the owner differs, even if the action is similar.

The deduplicated list is what the user will see next.

### 5. Confirm with the user before creating

**Show the dedup'd list and wait for a go-ahead.** Offer three options:

1. Create all of them as-is.
2. Create a named subset.
3. Edit titles / owners / descriptions first.

Never skip this step — automatic creation from a transcript is the single biggest source of tracker clutter this skill can cause.

### 6. Create one issue per item

Keep each ticket small — **one action, one ticket**. For each approved item:

```bash
devboy tools call create_issue '{
  "title": "Short imperative title (one line)",
  "description": "Source: meeting \"<meeting title>\" on <ISO date>.\nOwner committed: <name>.\n\nContext:\n<2–4 lines from the transcript or action_items entry>",
  "assignees": ["<handle>"],
  "labels": ["from-meeting"]
}'
```

Guidelines:

- Title is imperative and under ~80 chars. "Draft RFC for auth migration" not "We should probably think about the RFC".
- Description always cites the meeting title + date so the ticket is traceable back to its source.
- Assign to the committed owner when known. If unknown, leave unassigned and note the speaker in the body.

### 7. Optional: link issues that share a parent

If several of the new issues clearly belong to one epic the user named, link them:

```bash
devboy tools call link_issues '{
  "sourceIssueKey": "<new issue key>",
  "targetIssueKey": "<epic key>",
  "linkType": "relates_to"
}'
```

Use `relates_to` by default; only use `blocks` when the user stated an ordering.

### 8. Report back

Reply with:

- The meeting source (title + date).
- The deduplicated list of created tickets (one line each: `KEY — title`).
- Items that were discussed but **not** created (because the user dropped them), so the audit trail is complete.

## Success criteria

- Zero tickets are created before the user confirms the deduplicated list.
- Every created ticket cites the source meeting in its description.
- Each ticket is one action (no multi-bullet tickets). If the extracted item has two distinct actions, it was two items and should have been split during dedup.
- The count of created tickets matches the count of approved items — no silent drops, no silent extras.

## Guardrails

- **Never auto-create.** Even when the user says "just do it", show the list once and wait for one-key confirmation. One extra round-trip beats filing 20 bad tickets.
- **Do not re-create on re-run.** If the user re-runs the skill on the same meeting, use `get_issues` with the `from-meeting` label (plus the source-meeting citation in the body) to find already-created tickets before creating duplicates.
- **Respect privacy.** Do not paste full transcript quotes into ticket descriptions — 2–4 lines of context is enough, more is a PII leak in a system that is often more widely shared than the meeting itself.

## Non-goals

- Estimating or prioritising the created tickets. The skill files them; triage is a separate step.
- Cross-meeting aggregation. One meeting → one batch of tickets.
- Updating existing tickets based on a meeting. Use `update_issue` directly for that — this skill only creates.
