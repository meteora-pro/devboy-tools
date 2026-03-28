# DevBoy Tools Reference

Auto-generated from code. Run `devboy tools docs` to regenerate.

## Provider Support Matrix

| Provider | Git Repository | Issue Tracker | Epics |
|----------|:---:|:---:|:---:|
| **GitHub** | ✅ | ✅ | — |
| **GitLab** | ✅ | ✅ | — |
| **ClickUp** | — | ✅ | ✅ |
| **Jira** | — | ✅ | — |

## Git Repository Tools

Providers: GitHub, GitLab

### `get_merge_requests`

Get merge requests / pull requests from configured provider.

| Parameter | Type | Required | Description |
|-----------|------|:---:|-------------|
| `labels` | array | — | Filter by label names |
| `target_branch` | string | — | Filter by target branch |
| `state` | enum | — | Filter by state (default: open) |
| `limit` | number | — | Maximum results (default: 20) |
| `author` | string | — | Filter by author username |
| `source_branch` | string | — | Filter by source branch |

### `get_merge_request`

Get a single merge request by key (e.g., 'pr#123', 'mr#456').

| Parameter | Type | Required | Description |
|-----------|------|:---:|-------------|
| `key` | string | ✅ | MR/PR key |

### `get_merge_request_discussions`

Get discussions/review comments for a merge request with code positions.

| Parameter | Type | Required | Description |
|-----------|------|:---:|-------------|
| `key` | string | ✅ | MR/PR key |
| `offset` | number | — | Skip N discussions (default: 0) |
| `limit` | number | — | Max discussions (default: 20) |

### `get_merge_request_diffs`

Get file diffs for a merge request.

| Parameter | Type | Required | Description |
|-----------|------|:---:|-------------|
| `key` | string | ✅ | MR/PR key |

### `create_merge_request`

Create a new merge request (GitLab) or pull request (GitHub).

| Parameter | Type | Required | Description |
|-----------|------|:---:|-------------|
| `title` | string | ✅ | MR/PR title |
| `target_branch` | string | ✅ | Target branch |
| `source_branch` | string | ✅ | Source branch |
| `draft` | boolean | — | Create as draft (default: false) |
| `labels` | array | — | Labels |
| `reviewers` | array | — | Reviewers |
| `description` | string | — | MR/PR description |

### `create_merge_request_comment`

Add a comment to a merge request. Can be general or inline code review.

| Parameter | Type | Required | Description |
|-----------|------|:---:|-------------|
| `key` | string | ✅ | MR/PR key |
| `body` | string | ✅ | Comment text |
| `discussion_id` | string | — | Reply to existing discussion |
| `line` | number | — | Line number for inline comment |
| `file_path` | string | — | File path for inline comment |
| `line_type` | enum | — | Line type (default: new) |
| `commit_sha` | string | — | Commit SHA for inline comment |

### `get_pipeline`

Get CI/CD pipeline status for branch or MR/PR with job details.

| Parameter | Type | Required | Description |
|-----------|------|:---:|-------------|
| `mrKey` | string | — | MR/PR key (priority over branch) |
| `branch` | string | — | Branch name (default: main) |
| `includeFailedLogs` | boolean | — | Include error extraction for failed jobs (default: true) |

### `get_job_logs`

Get CI/CD job logs. Modes: smart (auto errors), search (pattern), paginated, full.

| Parameter | Type | Required | Description |
|-----------|------|:---:|-------------|
| `jobId` | string | ✅ | Job ID from get_pipeline |
| `offset` | number | — | Start line for paginated mode |
| `limit` | number | — | Lines to return (default: 200, max: 1000) |
| `full` | boolean | — | Return entire log |
| `pattern` | string | — | Regex/keyword search pattern |
| `context` | number | — | Context lines around match (default: 5) |
| `maxMatches` | number | — | Max search results (default: 20) |

## Issue Tracker Tools

Providers: GitHub, GitLab, ClickUp, Jira

### `get_issues`

Get issues from configured provider. Returns a list with filters.

| Parameter | Type | Required | Description |
|-----------|------|:---:|-------------|
| `limit` | number | — | Maximum number of results (default: 20) |
| `sort_by` | enum | — | Sort by field (default: updated_at) |
| `search` | string | — | Search query for title and description |
| `state` | enum | — | Filter by issue state (default: open) |
| `assignee` | string | — | Filter by assignee username |
| `offset` | number | — | Number of results to skip (default: 0) |
| `sort_order` | enum | — | Sort order (default: desc) |
| `labels` | array | — | Filter by label names |

### `get_issue`

Get a single issue by key (e.g., 'gh#123', 'gitlab#456', 'CU-abc', 'jira#PROJ-123').

| Parameter | Type | Required | Description |
|-----------|------|:---:|-------------|
| `key` | string | ✅ | Issue key |

### `get_issue_comments`

Get comments for an issue.

| Parameter | Type | Required | Description |
|-----------|------|:---:|-------------|
| `key` | string | ✅ | Issue key |

### `create_issue`

Create a new issue in the configured provider.

| Parameter | Type | Required | Description |
|-----------|------|:---:|-------------|
| `title` | string | ✅ | Issue title |
| `labels` | array | — | Labels to add |
| `description` | string | — | Issue description/body |
| `assignees` | array | — | Assignee usernames |

### `update_issue`

Update an existing issue. Only provided fields will be changed.

| Parameter | Type | Required | Description |
|-----------|------|:---:|-------------|
| `key` | string | ✅ | Issue key |
| `state` | enum | — | New state |
| `labels` | array | — | New labels (replaces existing) |
| `assignees` | array | — | New assignees |
| `description` | string | — | New description |
| `title` | string | — | New title |

### `add_issue_comment`

Add a comment to an issue.

| Parameter | Type | Required | Description |
|-----------|------|:---:|-------------|
| `key` | string | ✅ | Issue key |
| `body` | string | ✅ | Comment text |

### `get_available_statuses`

Get available statuses for the issue tracker.

No parameters.

### `get_users`

Get users from the issue tracker (Jira). Search by name, project, or ID.

| Parameter | Type | Required | Description |
|-----------|------|:---:|-------------|
| `maxResults` | number | — | Max results (default: 50) |
| `userId` | string | — | Get specific user by ID |
| `search` | string | — | Search by name or email |
| `projectKey` | string | — | Get assignable users for project |

### `link_issues`

Link two issues together (blocks, relates_to, etc.).

| Parameter | Type | Required | Description |
|-----------|------|:---:|-------------|
| `targetIssueKey` | string | ✅ | Target issue key |
| `sourceIssueKey` | string | ✅ | Source issue key |
| `linkType` | string | ✅ | Link type (e.g., blocks, relates_to) |

## Epics Tools

Providers: ClickUp

### `get_epics`

Get epics (high-level tasks) from the issue tracker.

| Parameter | Type | Required | Description |
|-----------|------|:---:|-------------|
| `search` | string | — | Search in epic title |
| `limit` | number | — | Max results (default: 50) |
| `offset` | number | — | Skip N results (default: 0) |

### `create_epic`

Create a new epic.

| Parameter | Type | Required | Description |
|-----------|------|:---:|-------------|
| `title` | string | ✅ | Epic title |
| `description` | string | — | Epic description |

### `update_epic`

Update an existing epic.

| Parameter | Type | Required | Description |
|-----------|------|:---:|-------------|
| `key` | string | ✅ | Epic key |
| `title` | string | — | New title |
| `description` | string | — | New description |

