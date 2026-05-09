---
name: solve-issue
description: Full cycle for shipping an issue — read it, branch, implement, push, open an MR, and link back to the original ticket.
category: issue-tracking
version: 1
compatibility: devboy-tools >= 0.18
activation:
  - "solve this issue"
  - "implement DEV-XXX"
  - "work on ticket"
  - "start this task"
  - "ship this issue"
tools:
  - get_issue
  - create_merge_request
  - add_issue_comment
---

# solve-issue

Take a ticket from "assigned" to "review-ready MR with a comment on the issue pointing at it". The skill wraps the typical loop: read the issue, cut a branch, do the work, push, open an MR on the configured Git provider, and leave a breadcrumb on the issue so the next person can follow the trail.

## When to use

- The user says "implement DEV-123", "solve this issue", "take this ticket", or similar.
- The agent has a clear, scoped issue to act on — not an open-ended discussion.
- A Git provider (GitLab or GitHub) is configured alongside the issue tracker.

If the request is vague, run `get-issues` first to surface the ticket, then come back here.

## Procedure

### 1. Read the issue

Always start with the full ticket — description, comments, links:

```bash
devboy tools call get_issue '{"key": "DEV-123"}'
```

Pay attention to:

- **Acceptance criteria** — they define "done".
- **Blocking links** — if `blocked_by` is non-empty, surface that and stop. The skill does not fight upstream blockers.
- **Recent comments** — later context often overrides the original description.

### 2. Name the branch

Pick a short, human-readable branch name that embeds the issue key:

```
feat/DEV-123-add-bulk-archive
fix/DEV-456-pipeline-badge-stale
chore/DEV-789-rotate-signing-keys
```

Rules:

- Prefix with `feat/`, `fix/`, `chore/`, `docs/`, or `refactor/` depending on the work.
- Include the issue key verbatim (no lowercasing — `DEV-123`, not `dev-123`).
- Keep the trailing slug under ~5 words.

### 3. Cut the branch and implement

Standard local Git flow — the skill does not wrap these in `devboy` tools. Base the branch off the remote's default head rather than hardcoding `main`, so the flow keeps working on repos whose default is `master` or something else:

```bash
git fetch origin
DEFAULT_BRANCH_REF="$(git symbolic-ref refs/remotes/origin/HEAD)"   # e.g. refs/remotes/origin/main
git switch -c feat/DEV-123-add-bulk-archive "$DEFAULT_BRANCH_REF"
# ... write the code, run tests, verify locally ...
git add -p
git commit -m "feat(issues): add bulk archive (DEV-123)"
git push -u origin HEAD
```

Commit messages should follow the repo's convention and end with the issue key in parentheses so the tracker auto-links them. Avoid `Fixes DEV-123` / `Closes #123` phrasing on the commit — see the guardrail below.

### 4. Open the merge request

`create_merge_request` is the unified tool — it opens a GitLab MR or a GitHub PR depending on the configured provider. Title and `source_branch` / `target_branch` are required. Set `target_branch` to the repo's actual default branch — do not hardcode `main`; derive it from `git symbolic-ref refs/remotes/origin/HEAD` (strip the `refs/remotes/origin/` prefix) so the call works on repos defaulted to `master` or a custom branch:

```bash
devboy tools call create_merge_request '{
  "title": "feat(issues): add bulk archive (DEV-123)",
  "description": "Implements DEV-123.\n\n## What changed\n- Added `bulkArchive` action to the issue list\n- New keyboard shortcut `shift+a`\n\n## How to test\n1. Select multiple issues\n2. Press `shift+a`\n3. Confirm they move to the archived state\n\nRelates to DEV-123.",
  "source_branch": "feat/DEV-123-add-bulk-archive",
  "target_branch": "<repo-default-branch>",
  "draft": false,
  "labels": ["feature"],
  "reviewers": ["alice"]
}'
```

Record the MR key (`mr#<n>` for GitLab, `pr#<n>` for GitHub) — the next step needs it.

### 5. Comment the MR link on the issue

Close the loop so anyone looking at the ticket finds the work:

```bash
devboy tools call add_issue_comment '{
  "key": "DEV-123",
  "body": "Opened for review: mr#42 — `feat/DEV-123-add-bulk-archive`."
}'
```

Include:

- The MR / PR key (clickable on most trackers when the integration is configured).
- The branch name.
- Any context the reviewer needs that is not already in the MR description.

### 6. Hand off

Report back to the user with:

- The issue key.
- The branch name.
- The MR / PR key and URL.
- A one-line summary of the change.

Then stop. Review happens in `review-mr`, fixing review comments happens in `fix-review-comments`.

## Guardrails

- **Do not auto-close the issue.** Leave the close to the merge commit. Most trackers have a `Fixes` keyword pipeline wired up on the MR description — use it there if the repo convention supports it, but do not call `update_issue` with `state: closed` from this skill.
- **Do not force-push shared branches.** If the branch already exists remotely and someone else has work on it, stop and ask.
- **Respect the scope.** The issue description is the contract — do not refactor adjacent code or add features the ticket did not ask for. If you find genuine tech debt, file a follow-up with `create-issue` instead of widening this change.
- **Do not run the review yourself.** That is a separate skill with a separate posture.

## Success criteria

- The MR exists on the configured Git provider, targets `main` (or the repo's default branch), and its description references the issue.
- The original issue has a new comment linking to the MR.
- Local branch is pushed and tracks its remote.
- The scope matches the issue's acceptance criteria — nothing more, nothing less.

## Non-goals

- **Code review.** Use `review-mr` on the resulting MR.
- **Applying review feedback.** Use `fix-review-comments` when reviewers come back.
- **Release / deploy coordination.** Out of scope — this skill stops at "MR open".
