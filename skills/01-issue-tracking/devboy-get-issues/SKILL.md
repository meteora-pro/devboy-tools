---
name: devboy-get-issues
description: Fetch and summarise issues from the configured tracker — filter, paginate, then drill into a single issue when needed.
category: issue-tracking
version: 1
compatibility: devboy-tools >= 0.18
activation:
  - "list issues"
  - "show open issues"
  - "fetch tickets"
  - "get issues"
  - "what's on the backlog"
tools:
  - get_issues
  - get_issue
  - get_issue_comments
  - get_issue_relations
  - get_available_statuses
---

# devboy-get-issues

Enumerate issues from whichever tracker is active (GitLab, GitHub, ClickUp, or Jira) and hand back either a compact summary of the result set or the full body of a single ticket the user cares about. The skill is always a read-only operation — it never mutates state.

## When to use

- The user asks "what issues are open?", "show me the backlog", or names a label / assignee they want to filter by.
- Another skill (e.g. `devboy-solve-issue`) needs to look up a specific ticket before acting on it.
- The agent is gathering context for a daily report or status update.

## Procedure

### 1. Scope the query

Most useful filters live directly on `get_issues`:

```bash
# Default: 20 most recently updated open issues
devboy tools call get_issues '{"state": "open", "limit": 20}'

# Narrow by label / assignee / free-text search
devboy tools call get_issues '{"state": "open", "labels": ["bug"], "assignee": "alice"}'
devboy tools call get_issues '{"search": "websocket reconnect", "limit": 10}'
```

Supported keys: `state` (`open` / `closed` / `all`), `search`, `labels`, `assignee`, `limit` (1–100), `offset`, `sort_by` (`created_at` / `updated_at`), `sort_order` (`asc` / `desc`), `projectKey`, `nativeQuery`. On Jira, `nativeQuery` accepts raw JQL and takes precedence over the other filters.

### 2. Paginate when the result set is large

`get_issues` returns at most 100 items per call. Walk the pages with `offset`:

```bash
devboy tools call get_issues '{"state": "open", "limit": 50, "offset": 0}'
devboy tools call get_issues '{"state": "open", "limit": 50, "offset": 50}'
```

Stop once a page returns fewer items than `limit`.

### 3. Summarise the set

Before dumping the raw list to the user, produce a compact roll-up:

- Total count, grouped by state (`open` / `closed`).
- Top labels and assignees by frequency.
- Any `priority` or status values that cluster (e.g. "4 urgent, 12 normal").

If the user asked a qualitative question ("is anything blocking the release?"), prioritise issues with `blocked`, `urgent`, or `in_progress` status over a flat listing.

### 4. Drill into a single issue

Once the user picks one, fetch the full record — including comments and relations in a single call:

```bash
devboy tools call get_issue '{"key": "DEV-123"}'
# or, to skip related chatter
devboy tools call get_issue '{"key": "DEV-123", "includeComments": false, "includeRelations": false}'
```

For heavier comment threads, page them separately:

```bash
devboy tools call get_issue_comments '{"key": "DEV-123"}'
devboy tools call get_issue_relations '{"key": "DEV-123"}'
```

### 5. (Optional) Enumerate tracker statuses

If the user asks "what states can an issue be in?", surface the tracker's workflow:

```bash
devboy tools call get_available_statuses
```

This is handy before calling `devboy-update-issue` on a provider whose state vocabulary is not "open / closed".

## Success criteria

- The filtered result set matches what the user asked for (state, label, assignee).
- The summary names the total count and the top groupings, not just the first page.
- For any issue the user inspects, the detail call returns real fields — not `ProviderUnsupported` or an empty payload.

## Non-goals

- This skill does not create, update, or close issues — use `devboy-create-issue` / `devboy-update-issue`.
- It does not start a branch or MR — that is `devboy-solve-issue`.
