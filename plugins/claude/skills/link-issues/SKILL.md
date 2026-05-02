---
name: link-issues
description: Connect two issues with a typed relationship — blocks, blocked-by, related, duplicates, or parent/subtask.
category: issue-tracking
version: 1
compatibility: devboy-tools >= 0.18
activation:
  - "link issues"
  - "mark as blocked by"
  - "set parent"
  - "relate to"
  - "mark as duplicate"
tools:
  - get_issue
  - get_issue_relations
  - link_issues
  - unlink_issues
  - update_issue
---

# link-issues

Create a typed relationship between two issues so the tracker's dependency graph reflects the real one. The skill covers the four canonical link types plus the special case of parent/subtask, and it inspects existing links first to avoid duplicates.

## When to use

- The user says "DEV-200 is blocked by DEV-100", "mark DEV-55 as a duplicate of DEV-40", or "make this a subtask of the epic".
- A fix unblocks another ticket and the blocker relationship should be recorded before the close.
- Back-office cleanup: stitching together issues that were filed separately but describe the same work.

## Key concepts

`IssueRelations` (the shape returned by `get_issue_relations`) groups links into five buckets:

| Bucket | `linkType` to pass | Meaning |
|--------|---------------------|---------|
| `blocked_by` | `blocked_by` | The source cannot progress until the target is done |
| `blocks` | `blocks` | The source must be done before the target can progress |
| `related_to` | `relates_to` | Soft relationship — "see also" |
| `duplicates` | `duplicates` | The source is a duplicate of the target |
| `parent` / `subtasks` | `subtask` | Source is a subtask of target (ClickUp / Jira) |

Provider support varies:

- **GitLab** exposes `relates_to`, `blocks`, `blocked_by`.
- **GitHub** exposes a flat `relates_to` (dependency-free by default; Projects v2 adds more).
- **ClickUp** exposes parent/subtask via `update_issue` `parentId` in addition to `link_issues`.
- **Jira** exposes the full set and honours whatever link types are configured on the project.

If a given relationship is not supported by the active provider, `link_issues` returns `ProviderUnsupported` — fall back to an explanatory comment instead.

## Procedure

### 1. Inspect the current graph

Before adding a link, list the existing ones so you do not create a duplicate edge:

```bash
devboy tools call get_issue_relations '{"key": "DEV-200"}'
# or in aggregate via get_issue
devboy tools call get_issue '{"key": "DEV-200", "includeRelations": true}'
```

Look at `blocked_by`, `blocks`, `related_to`, `duplicates`, `parent`, `subtasks`. If the edge you are about to add already exists, stop — the tracker is already correct.

### 2. Add the link

One `link_issues` call per edge. All three fields are required:

```bash
# "DEV-200 is blocked by DEV-100"
devboy tools call link_issues '{
  "sourceIssueKey": "DEV-200",
  "targetIssueKey": "DEV-100",
  "linkType": "blocked_by"
}'

# "DEV-55 duplicates DEV-40"
devboy tools call link_issues '{
  "sourceIssueKey": "DEV-55",
  "targetIssueKey": "DEV-40",
  "linkType": "duplicates"
}'

# "DEV-42 is related to DEV-17"
devboy tools call link_issues '{
  "sourceIssueKey": "DEV-42",
  "targetIssueKey": "DEV-17",
  "linkType": "relates_to"
}'
```

### 3. Parent / subtask is a special case

Where the provider supports it, prefer `update_issue` with `parentId` — it moves the issue under the parent in one call and most trackers render the hierarchy natively:

```bash
devboy tools call update_issue '{"key": "CU-child", "parentId": "CU-epic"}'
```

Fall back to `link_issues` with `"linkType": "subtask"` if the provider expects that shape.

### 4. Multi-link sequences

A real request usually needs more than one edge. Walk them one at a time — the tool is not batched:

> "This MR fixes DEV-123 and is blocked by DEV-100."

```bash
# DEV-<current> blocks? no — DEV-<current> is blocked by DEV-100
devboy tools call link_issues '{
  "sourceIssueKey": "DEV-<current>",
  "targetIssueKey": "DEV-100",
  "linkType": "blocked_by"
}'
# "fixes" on most trackers is just a related link + automation on merge
devboy tools call link_issues '{
  "sourceIssueKey": "DEV-<current>",
  "targetIssueKey": "DEV-123",
  "linkType": "relates_to"
}'
```

### 5. Removing a stale link

`unlink_issues` takes the same three fields:

```bash
devboy tools call unlink_issues '{
  "sourceIssueKey": "DEV-55",
  "targetIssueKey": "DEV-40",
  "linkType": "duplicates"
}'
```

## Success criteria

- `get_issue_relations` after the operation shows the new edge in the correct bucket.
- No duplicate edges were created — pre-check in step 1 caught them.
- Where the provider rejected a link type, the agent surfaced that to the user instead of silently retrying.

## Non-goals

- This skill does not create issues — use `devboy-create-issue` and then link.
- It does not automatically close duplicates — mark the relationship, let the user decide whether to close.
