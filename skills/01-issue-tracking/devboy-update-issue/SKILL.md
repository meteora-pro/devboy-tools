---
name: devboy-update-issue
description: Change an issue's state, labels, assignees, or priority with a partial update, leaving unrelated fields untouched.
category: issue-tracking
version: 1
compatibility: devboy-tools >= 0.18
activation:
  - "close this issue"
  - "update the ticket"
  - "move to done"
  - "assign to X"
  - "relabel issue"
  - "reopen ticket"
tools:
  - get_issue
  - update_issue
  - add_issue_comment
  - get_available_statuses
---

# devboy-update-issue

Apply a targeted change to an existing issue — transition state, swap labels, hand it to someone else, bump priority — without disturbing fields the user did not mention. The skill always inspects the current record first so the update is informed.

## When to use

- The user says "close DEV-123", "move this to in progress", "assign to @alice", or similar.
- Another skill finished work and needs to transition the originating ticket (but see the guardrail on auto-closing in `devboy-solve-issue`).
- A label taxonomy change requires bulk relabelling — run the skill once per ticket.

## Procedure

### 1. Read the current state first

Never update blind. Fetch the issue so you know what you are overwriting:

```bash
devboy tools call get_issue '{"key": "DEV-123"}'
```

Note the current `state`, `labels`, `assignees`, and `priority` before deciding on the diff. This also surfaces whether the issue is already in the desired state (in which case no call is needed).

### 2. Understand the tracker's state vocabulary

GitLab / GitHub use `open` / `closed`. ClickUp and Jira use custom workflows — "To Do", "In Progress", "In Review", "Done", "Blocked". When the user asks for something semantic ("mark it done"), check what the tracker actually offers:

```bash
devboy tools call get_available_statuses
```

Pick the status that matches the user's intent. If none fits, ask before guessing.

### 3. Apply the minimal update

`update_issue` is **partial** — fields you omit are preserved. Send only what actually changes:

```bash
# Close an issue
devboy tools call update_issue '{"key": "DEV-123", "state": "closed"}'

# Reassign without touching anything else
devboy tools call update_issue '{"key": "DEV-123", "assignees": ["bob"]}'

# Swap the label set (replaces — not additive)
devboy tools call update_issue '{"key": "DEV-123", "labels": ["bug", "regression"]}'

# Transition a ClickUp / Jira status by string — pass the literal status name
devboy tools call update_issue '{"key": "CU-abc123", "status": "In Review"}'
```

Tool fields you can pass: `title`, `description`, `state`, `labels`, `assignees`, `parentId` (ClickUp subtasks), `markdown`. `labels` and `assignees` are replacements, not merges — re-send the full desired list.

### 4. Narrate the change in a comment

When the state change carries meaning the field itself does not capture, follow up with a comment so the history is readable:

```bash
devboy tools call add_issue_comment '{
  "key": "DEV-123",
  "body": "Moving to **Blocked** — waiting on vendor fix for upstream auth (ETA Friday)."
}'
```

Good triggers for a comment: state → blocked, reassignment, priority escalation, a close-as-wontfix that needs context.

### 5. Verify

Re-read the issue and confirm the intended fields changed while the others did not:

```bash
devboy tools call get_issue '{"key": "DEV-123", "includeComments": false, "includeRelations": false}'
```

## Guardrails

- **Never overwrite fields the user did not explicitly mention.** If the user said "close it", do not also clear labels or drop assignees. Send only the fields that matter for the stated intent.
- **Do not batch unrelated changes into a single tool call** if the user can realistically reject one and accept the other — make each change auditable.
- **Status names are case-sensitive** on some providers. Use the exact casing from `get_available_statuses`.

## Success criteria

- The targeted fields reflect the user's request after step 5.
- All other fields remain identical to the pre-update snapshot from step 1.
- Where the change needs context, there is a comment explaining it.
