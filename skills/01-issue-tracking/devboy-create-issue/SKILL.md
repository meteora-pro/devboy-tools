---
name: devboy-create-issue
description: Create a well-structured issue in the configured tracker using Feature, Defect, or Task templates.
category: issue-tracking
version: 1
compatibility: devboy-tools >= 0.18
activation:
  - "create an issue"
  - "file a bug"
  - "open a ticket"
  - "make a new ticket"
  - "add a task"
tools:
  - create_issue
  - add_issue_comment
  - get_available_statuses
  - get_users
---

# devboy-create-issue

Turn a free-form user request into a structured ticket the team can actually work on. The skill picks one of three templates, fills it, creates the issue, and (optionally) attaches reproduction details or logs as a follow-up comment.

## When to use

- The user describes a bug, a feature idea, or a piece of work without a ticket yet.
- Another skill discovered something worth filing (e.g. `devboy-review-mr` flagged a deeper issue).
- The user asks to "open a ticket for this" in the middle of a longer conversation.

## Procedure

### 1. Pick the template

Classify the request before writing anything. The three templates share the same shape — swap the word and tweak the slots.

**Feature** — new capability, user-visible change, or design work.

```markdown
## Title
<verb + concrete outcome>, e.g. "Add bulk archive to issue list"

## Context
Who asked for this and why. Link the user conversation or meeting note.

## Acceptance criteria
- [ ] Observable behaviour #1
- [ ] Observable behaviour #2
- [ ] Docs / changelog updated

## Notes
Design sketches, open questions, out-of-scope.
```

**Defect** — something is demonstrably broken.

```markdown
## Title
<component>: <what breaks>, e.g. "Dashboard: pipeline badge shows stale status after retry"

## Context
Environment, version, user impact. Link the failing run / screenshot / log if there is one.

## Acceptance criteria
- [ ] Steps to reproduce no longer trigger the bug
- [ ] Regression test covers the fix
- [ ] Release note drafted

## Notes
Suspected root cause, related incidents.
```

**Task** — chore, refactor, docs, infra, anything that is not a user-facing feature and not a bug.

```markdown
## Title
<scope>: <short outcome>, e.g. "Infra: rotate CI signing keys"

## Context
Why now, what it unblocks.

## Acceptance criteria
- [ ] The concrete thing is done
- [ ] Follow-up tickets filed for anything we deliberately punted

## Notes
```

### 2. Create the issue

Pass the title and the rendered Markdown body to `create_issue`:

```bash
devboy tools call create_issue '{
  "title": "Dashboard: pipeline badge shows stale status after retry",
  "description": "## Context\nSeen by @alice on prod v2.4.1. The badge keeps the first status after a manual retry.\n\n## Acceptance criteria\n- [ ] Badge refreshes within 5s of retry\n- [ ] Regression test covers the fix\n\n## Notes\nSuspected cache miss in `PipelineStatusService`.",
  "labels": ["bug", "dashboard"],
  "assignees": ["alice"]
}'
```

On Windows, JSON needs escaped quotes:

```cmd
devboy tools call create_issue "{\"title\": \"...\", \"description\": \"...\"}"
```

Useful optional fields: `parentId` (ClickUp subtasks), `issueType` (Jira: `Task` / `Bug` / `Story`), `projectId` (Jira project key override), `markdown: false` when the description is plain text.

### 3. Attach reproduction details as a comment

Keep the description tight — offload long logs, stack traces, or screenshots to a comment. `add_issue_comment` takes the key returned by step 2:

```bash
devboy tools call add_issue_comment '{
  "key": "DEV-789",
  "body": "Full stack trace from staging:\n\n```\nReferenceError: ...\n```"
}'
```

On ClickUp, the same tool accepts `attachments` for small files (each ≤ 10 MB, base64-encoded).

### 4. Confirm the result

Read the new issue back so the user sees exactly what was filed:

```bash
devboy tools call get_issue '{"key": "DEV-789"}'
```

## Success criteria

- The created issue has a clear title, a populated body following the chosen template, and the right labels / assignees.
- Any reproduction material lives in a comment rather than bloating the description.
- The agent reports the new issue key back to the user so they can link it elsewhere.

## Non-goals

- This skill does not triage or reassign existing issues — that is `devboy-update-issue`.
- It does not link issues together — that is `devboy-link-issues`.
