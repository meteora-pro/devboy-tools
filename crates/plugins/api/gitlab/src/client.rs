//! GitLab API client implementation.

use async_trait::async_trait;
use devboy_core::{
    CodePosition, Comment, CreateCommentInput, CreateIssueInput, Discussion, Error, FileDiff,
    Issue, IssueFilter, IssueProvider, MergeRequest, MergeRequestProvider, MrFilter, Provider,
    Result, UpdateIssueInput, User,
};
use tracing::{debug, warn};

use crate::types::{
    CreateDiscussionRequest, CreateIssueRequest, CreateNoteRequest, DiscussionPosition, GitLabDiff,
    GitLabDiscussion, GitLabIssue, GitLabMergeRequest, GitLabMergeRequestChanges, GitLabNote,
    GitLabNotePosition, GitLabUser, UpdateIssueRequest,
};
use crate::DEFAULT_GITLAB_URL;

/// GitLab API client.
pub struct GitLabClient {
    base_url: String,
    project_id: String,
    token: String,
    client: reqwest::Client,
}

impl GitLabClient {
    /// Create a new GitLab client.
    pub fn new(project_id: impl Into<String>, token: impl Into<String>) -> Self {
        Self::with_base_url(DEFAULT_GITLAB_URL, project_id, token)
    }

    /// Create a new GitLab client with a custom base URL.
    pub fn with_base_url(
        base_url: impl Into<String>,
        project_id: impl Into<String>,
        token: impl Into<String>,
    ) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            project_id: project_id.into(),
            token: token.into(),
            client: reqwest::Client::new(),
        }
    }

    /// Build request with common headers.
    fn request(&self, method: reqwest::Method, url: &str) -> reqwest::RequestBuilder {
        self.client
            .request(method, url)
            .header("PRIVATE-TOKEN", &self.token)
    }

    /// Get the project API URL for a given endpoint.
    fn project_url(&self, endpoint: &str) -> String {
        format!(
            "{}/api/v4/projects/{}{}",
            self.base_url, self.project_id, endpoint
        )
    }

    /// Get the API URL for a given endpoint (non-project-scoped).
    fn api_url(&self, endpoint: &str) -> String {
        format!("{}/api/v4{}", self.base_url, endpoint)
    }

    /// Make an authenticated GET request with typed deserialization.
    async fn get<T: serde::de::DeserializeOwned>(&self, url: &str) -> Result<T> {
        debug!(url = url, "GitLab GET request");

        let response = self
            .request(reqwest::Method::GET, url)
            .send()
            .await
            .map_err(|e| Error::Http(e.to_string()))?;

        self.handle_response(response).await
    }

    /// Make an authenticated POST request.
    async fn post<T: serde::de::DeserializeOwned, B: serde::Serialize>(
        &self,
        url: &str,
        body: &B,
    ) -> Result<T> {
        debug!(url = url, "GitLab POST request");

        let response = self
            .request(reqwest::Method::POST, url)
            .json(body)
            .send()
            .await
            .map_err(|e| Error::Http(e.to_string()))?;

        self.handle_response(response).await
    }

    /// Make an authenticated PUT request.
    async fn put<T: serde::de::DeserializeOwned, B: serde::Serialize>(
        &self,
        url: &str,
        body: &B,
    ) -> Result<T> {
        debug!(url = url, "GitLab PUT request");

        let response = self
            .request(reqwest::Method::PUT, url)
            .json(body)
            .send()
            .await
            .map_err(|e| Error::Http(e.to_string()))?;

        self.handle_response(response).await
    }

    /// Handle response and map errors.
    async fn handle_response<T: serde::de::DeserializeOwned>(
        &self,
        response: reqwest::Response,
    ) -> Result<T> {
        let status = response.status();

        if !status.is_success() {
            let status_code = status.as_u16();
            let message = response.text().await.unwrap_or_default();
            warn!(
                status = status_code,
                message = message,
                "GitLab API error response"
            );
            return Err(Error::from_status(status_code, message));
        }

        response
            .json()
            .await
            .map_err(|e| Error::InvalidData(format!("Failed to parse response: {}", e)))
    }
}

// =============================================================================
// Mapping functions: GitLab types -> Unified types
// =============================================================================

fn map_user(gl_user: Option<&GitLabUser>) -> Option<User> {
    gl_user.map(|u| User {
        id: u.id.to_string(),
        username: u.username.clone(),
        name: u.name.clone(),
        email: None, // GitLab doesn't return email in most contexts
        avatar_url: u.avatar_url.clone(),
    })
}

fn map_user_required(gl_user: Option<&GitLabUser>) -> User {
    map_user(gl_user).unwrap_or_else(|| User {
        id: "unknown".to_string(),
        username: "unknown".to_string(),
        name: Some("Unknown".to_string()),
        ..Default::default()
    })
}

fn map_issue(gl_issue: &GitLabIssue) -> Issue {
    Issue {
        key: format!("gitlab#{}", gl_issue.iid),
        title: gl_issue.title.clone(),
        description: gl_issue.description.clone(),
        state: gl_issue.state.clone(),
        source: "gitlab".to_string(),
        priority: None, // GitLab doesn't have built-in priority
        labels: gl_issue.labels.clone(),
        author: map_user(gl_issue.author.as_ref()),
        assignees: gl_issue
            .assignees
            .iter()
            .map(|u| map_user_required(Some(u)))
            .collect(),
        url: Some(gl_issue.web_url.clone()),
        created_at: Some(gl_issue.created_at.clone()),
        updated_at: Some(gl_issue.updated_at.clone()),
    }
}

fn map_merge_request(gl_mr: &GitLabMergeRequest) -> MergeRequest {
    // Determine state: check merged_at first, then closed, then draft
    let state = if gl_mr.merged_at.is_some() {
        "merged".to_string()
    } else if gl_mr.state == "closed" {
        "closed".to_string()
    } else if gl_mr.draft || gl_mr.work_in_progress {
        "draft".to_string()
    } else {
        gl_mr.state.clone() // "opened" etc.
    };

    MergeRequest {
        key: format!("mr#{}", gl_mr.iid),
        title: gl_mr.title.clone(),
        description: gl_mr.description.clone(),
        state,
        source: "gitlab".to_string(),
        source_branch: gl_mr.source_branch.clone(),
        target_branch: gl_mr.target_branch.clone(),
        author: map_user(gl_mr.author.as_ref()),
        assignees: gl_mr
            .assignees
            .iter()
            .map(|u| map_user_required(Some(u)))
            .collect(),
        reviewers: gl_mr
            .reviewers
            .iter()
            .map(|u| map_user_required(Some(u)))
            .collect(),
        labels: gl_mr.labels.clone(),
        draft: gl_mr.draft || gl_mr.work_in_progress,
        url: Some(gl_mr.web_url.clone()),
        created_at: Some(gl_mr.created_at.clone()),
        updated_at: Some(gl_mr.updated_at.clone()),
    }
}

fn map_note(gl_note: &GitLabNote) -> Comment {
    let position = gl_note.position.as_ref().and_then(map_position);

    Comment {
        id: gl_note.id.to_string(),
        body: gl_note.body.clone(),
        author: map_user(gl_note.author.as_ref()),
        created_at: Some(gl_note.created_at.clone()),
        updated_at: gl_note.updated_at.clone(),
        position,
    }
}

fn map_position(gl_position: &GitLabNotePosition) -> Option<CodePosition> {
    // Determine file path and line based on position type
    let (file_path, line, line_type) = if let Some(new_line) = gl_position.new_line {
        let path = gl_position
            .new_path
            .clone()
            .unwrap_or_else(|| gl_position.old_path.clone().unwrap_or_default());
        (path, new_line, "new".to_string())
    } else if let Some(old_line) = gl_position.old_line {
        let path = gl_position
            .old_path
            .clone()
            .unwrap_or_else(|| gl_position.new_path.clone().unwrap_or_default());
        (path, old_line, "old".to_string())
    } else {
        return None;
    };

    Some(CodePosition {
        file_path,
        line,
        line_type,
        commit_sha: None,
    })
}

fn map_discussion(gl_discussion: &GitLabDiscussion) -> Discussion {
    // Filter out system notes
    let notes: Vec<&GitLabNote> = gl_discussion.notes.iter().filter(|n| !n.system).collect();

    if notes.is_empty() {
        return Discussion {
            id: gl_discussion.id.clone(),
            resolved: false,
            resolved_by: None,
            comments: vec![],
            position: None,
        };
    }

    let comments: Vec<Comment> = notes.iter().map(|n| map_note(n)).collect();
    let position = comments.first().and_then(|c| c.position.clone());

    // Check resolved status from the first resolvable note
    let first_resolvable = notes.iter().find(|n| n.resolvable);
    let resolved = first_resolvable.is_some_and(|n| n.resolved);
    let resolved_by = first_resolvable.and_then(|n| map_user(n.resolved_by.as_ref()));

    Discussion {
        id: gl_discussion.id.clone(),
        resolved,
        resolved_by,
        comments,
        position,
    }
}

fn map_diff(gl_diff: &GitLabDiff) -> FileDiff {
    FileDiff {
        file_path: gl_diff.new_path.clone(),
        old_path: if gl_diff.renamed_file {
            Some(gl_diff.old_path.clone())
        } else {
            None
        },
        new_file: gl_diff.new_file,
        deleted_file: gl_diff.deleted_file,
        renamed_file: gl_diff.renamed_file,
        diff: gl_diff.diff.clone(),
        additions: None, // GitLab diff endpoint doesn't provide line counts
        deletions: None,
    }
}

// =============================================================================
// Helper functions
// =============================================================================

/// Parse issue key like "gitlab#123" to get issue iid.
fn parse_issue_key(key: &str) -> Result<u64> {
    key.strip_prefix("gitlab#")
        .and_then(|s| s.parse::<u64>().ok())
        .ok_or_else(|| Error::InvalidData(format!("Invalid issue key: {}", key)))
}

/// Parse MR key like "mr#123" to get MR iid.
fn parse_mr_key(key: &str) -> Result<u64> {
    key.strip_prefix("mr#")
        .and_then(|s| s.parse::<u64>().ok())
        .ok_or_else(|| Error::InvalidData(format!("Invalid MR key: {}", key)))
}

// =============================================================================
// Trait implementations
// =============================================================================

#[async_trait]
impl IssueProvider for GitLabClient {
    async fn get_issues(&self, filter: IssueFilter) -> Result<Vec<Issue>> {
        let mut url = self.project_url("/issues");
        let mut params = vec![];

        if let Some(state) = &filter.state {
            let gl_state = match state.as_str() {
                "open" | "opened" => "opened",
                "closed" => "closed",
                "all" => "all",
                _ => "opened",
            };
            params.push(format!("state={}", gl_state));
        }

        if let Some(search) = &filter.search {
            params.push(format!("search={}", search));
        }

        if let Some(labels) = &filter.labels {
            if !labels.is_empty() {
                params.push(format!("labels={}", labels.join(",")));
            }
        }

        if let Some(assignee) = &filter.assignee {
            params.push(format!("assignee_username={}", assignee));
        }

        if let Some(limit) = filter.limit {
            params.push(format!("per_page={}", limit.min(100)));
        }

        if let Some(offset) = filter.offset {
            let per_page = filter.limit.unwrap_or(20);
            let page = (offset / per_page) + 1;
            params.push(format!("page={}", page));
        }

        if let Some(sort_by) = &filter.sort_by {
            let gl_sort = match sort_by.as_str() {
                "created_at" | "created" => "created_at",
                "updated_at" | "updated" => "updated_at",
                _ => "updated_at",
            };
            params.push(format!("order_by={}", gl_sort));
        }

        if let Some(order) = &filter.sort_order {
            params.push(format!("sort={}", order));
        }

        if !params.is_empty() {
            url.push_str(&format!("?{}", params.join("&")));
        }

        let gl_issues: Vec<GitLabIssue> = self.get(&url).await?;
        Ok(gl_issues.iter().map(map_issue).collect())
    }

    async fn get_issue(&self, key: &str) -> Result<Issue> {
        let iid = parse_issue_key(key)?;
        let url = self.project_url(&format!("/issues/{}", iid));
        let gl_issue: GitLabIssue = self.get(&url).await?;
        Ok(map_issue(&gl_issue))
    }

    async fn create_issue(&self, input: CreateIssueInput) -> Result<Issue> {
        let url = self.project_url("/issues");
        let labels = if input.labels.is_empty() {
            None
        } else {
            Some(input.labels.join(","))
        };

        let request = CreateIssueRequest {
            title: input.title,
            description: input.description,
            labels,
            assignee_ids: None, // GitLab needs user IDs, not usernames; skip for now
        };

        let gl_issue: GitLabIssue = self.post(&url, &request).await?;
        Ok(map_issue(&gl_issue))
    }

    async fn update_issue(&self, key: &str, input: UpdateIssueInput) -> Result<Issue> {
        let iid = parse_issue_key(key)?;
        let url = self.project_url(&format!("/issues/{}", iid));

        // Map state to state_event
        let state_event = input.state.map(|s| match s.as_str() {
            "opened" | "open" => "reopen".to_string(),
            "closed" | "close" => "close".to_string(),
            _ => s,
        });

        let labels = input.labels.map(|l| l.join(","));

        let request = UpdateIssueRequest {
            title: input.title,
            description: input.description,
            state_event,
            labels,
            assignee_ids: None,
        };

        let gl_issue: GitLabIssue = self.put(&url, &request).await?;
        Ok(map_issue(&gl_issue))
    }

    async fn get_comments(&self, issue_key: &str) -> Result<Vec<Comment>> {
        let iid = parse_issue_key(issue_key)?;
        let url = self.project_url(&format!("/issues/{}/notes", iid));
        let gl_notes: Vec<GitLabNote> = self.get(&url).await?;

        // Filter out system notes
        Ok(gl_notes
            .iter()
            .filter(|n| !n.system)
            .map(map_note)
            .collect())
    }

    async fn add_comment(&self, issue_key: &str, body: &str) -> Result<Comment> {
        let iid = parse_issue_key(issue_key)?;
        let url = self.project_url(&format!("/issues/{}/notes", iid));
        let request = CreateNoteRequest {
            body: body.to_string(),
        };

        let gl_note: GitLabNote = self.post(&url, &request).await?;
        Ok(map_note(&gl_note))
    }

    fn provider_name(&self) -> &'static str {
        "gitlab"
    }
}

#[async_trait]
impl MergeRequestProvider for GitLabClient {
    async fn get_merge_requests(&self, filter: MrFilter) -> Result<Vec<MergeRequest>> {
        let mut url = self.project_url("/merge_requests");
        let mut params = vec![];

        if let Some(state) = &filter.state {
            let gl_state = match state.as_str() {
                "open" | "opened" => "opened",
                "closed" => "closed",
                "merged" => "merged",
                "all" => "all",
                _ => "opened",
            };
            params.push(format!("state={}", gl_state));
        }

        if let Some(source_branch) = &filter.source_branch {
            params.push(format!("source_branch={}", source_branch));
        }

        if let Some(target_branch) = &filter.target_branch {
            params.push(format!("target_branch={}", target_branch));
        }

        if let Some(author) = &filter.author {
            params.push(format!("author_username={}", author));
        }

        if let Some(labels) = &filter.labels {
            if !labels.is_empty() {
                params.push(format!("labels={}", labels.join(",")));
            }
        }

        if let Some(limit) = filter.limit {
            params.push(format!("per_page={}", limit.min(100)));
        }

        params.push("order_by=updated_at".to_string());
        params.push("sort=desc".to_string());

        if !params.is_empty() {
            url.push_str(&format!("?{}", params.join("&")));
        }

        let gl_mrs: Vec<GitLabMergeRequest> = self.get(&url).await?;
        Ok(gl_mrs.iter().map(map_merge_request).collect())
    }

    async fn get_merge_request(&self, key: &str) -> Result<MergeRequest> {
        let iid = parse_mr_key(key)?;
        let url = self.project_url(&format!("/merge_requests/{}", iid));
        let gl_mr: GitLabMergeRequest = self.get(&url).await?;
        Ok(map_merge_request(&gl_mr))
    }

    async fn get_discussions(&self, mr_key: &str) -> Result<Vec<Discussion>> {
        let iid = parse_mr_key(mr_key)?;
        let url = self.project_url(&format!("/merge_requests/{}/discussions", iid));
        let gl_discussions: Vec<GitLabDiscussion> = self.get(&url).await?;

        // Map and filter out empty discussions (all system notes)
        Ok(gl_discussions
            .iter()
            .map(map_discussion)
            .filter(|d| !d.comments.is_empty())
            .collect())
    }

    async fn get_diffs(&self, mr_key: &str) -> Result<Vec<FileDiff>> {
        let iid = parse_mr_key(mr_key)?;
        // Use the changes endpoint which returns diffs with content
        let url = self.project_url(&format!("/merge_requests/{}/changes", iid));
        let gl_changes: GitLabMergeRequestChanges = self.get(&url).await?;
        Ok(gl_changes.changes.iter().map(map_diff).collect())
    }

    async fn add_comment(&self, mr_key: &str, input: CreateCommentInput) -> Result<Comment> {
        let iid = parse_mr_key(mr_key)?;

        // If discussion_id is provided, reply to existing discussion
        if let Some(discussion_id) = &input.discussion_id {
            let url = self.project_url(&format!(
                "/merge_requests/{}/discussions/{}/notes",
                iid, discussion_id
            ));
            let request = CreateNoteRequest { body: input.body };
            let gl_note: GitLabNote = self.post(&url, &request).await?;
            return Ok(map_note(&gl_note));
        }

        // If position is provided, create inline discussion
        if let Some(position) = &input.position {
            // Need diff_refs from the MR to create inline comments
            let mr_url = self.project_url(&format!("/merge_requests/{}", iid));
            let gl_mr: GitLabMergeRequest = self.get(&mr_url).await?;

            let diff_refs = gl_mr.diff_refs.ok_or_else(|| {
                Error::InvalidData("MR has no diff_refs, cannot create inline comment".to_string())
            })?;

            let (new_line, old_line, new_path, old_path) = if position.line_type == "old" {
                (
                    None,
                    Some(position.line),
                    None,
                    Some(position.file_path.clone()),
                )
            } else {
                (
                    Some(position.line),
                    None,
                    Some(position.file_path.clone()),
                    None,
                )
            };

            let url = self.project_url(&format!("/merge_requests/{}/discussions", iid));
            let request = CreateDiscussionRequest {
                body: input.body,
                position: Some(DiscussionPosition {
                    position_type: "text".to_string(),
                    base_sha: diff_refs.base_sha,
                    start_sha: diff_refs.start_sha,
                    head_sha: diff_refs.head_sha,
                    new_path,
                    old_path,
                    new_line,
                    old_line,
                }),
            };

            let gl_discussion: GitLabDiscussion = self.post(&url, &request).await?;
            let first_note = gl_discussion.notes.first().ok_or_else(|| {
                Error::InvalidData("Discussion created with no notes".to_string())
            })?;
            return Ok(map_note(first_note));
        }

        // General comment (note) on the MR
        let url = self.project_url(&format!("/merge_requests/{}/notes", iid));
        let request = CreateNoteRequest { body: input.body };

        let gl_note: GitLabNote = self.post(&url, &request).await?;
        Ok(map_note(&gl_note))
    }

    fn provider_name(&self) -> &'static str {
        "gitlab"
    }
}

#[async_trait]
impl Provider for GitLabClient {
    async fn get_current_user(&self) -> Result<User> {
        let url = self.api_url("/user");
        let gl_user: GitLabUser = self.get(&url).await?;
        Ok(map_user_required(Some(&gl_user)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{GitLabDiffRefs, GitLabNotePosition};

    #[test]
    fn test_parse_issue_key() {
        assert_eq!(parse_issue_key("gitlab#123").unwrap(), 123);
        assert_eq!(parse_issue_key("gitlab#1").unwrap(), 1);
        assert!(parse_issue_key("mr#123").is_err());
        assert!(parse_issue_key("gh#123").is_err());
        assert!(parse_issue_key("123").is_err());
        assert!(parse_issue_key("gitlab#").is_err());
    }

    #[test]
    fn test_parse_mr_key() {
        assert_eq!(parse_mr_key("mr#456").unwrap(), 456);
        assert_eq!(parse_mr_key("mr#1").unwrap(), 1);
        assert!(parse_mr_key("gitlab#123").is_err());
        assert!(parse_mr_key("pr#123").is_err());
        assert!(parse_mr_key("456").is_err());
    }

    #[test]
    fn test_map_user() {
        let gl_user = GitLabUser {
            id: 42,
            username: "testuser".to_string(),
            name: Some("Test User".to_string()),
            avatar_url: Some("https://gitlab.com/avatar.png".to_string()),
            web_url: Some("https://gitlab.com/testuser".to_string()),
        };

        let user = map_user(Some(&gl_user)).unwrap();
        assert_eq!(user.id, "42");
        assert_eq!(user.username, "testuser");
        assert_eq!(user.name, Some("Test User".to_string()));
        assert_eq!(
            user.avatar_url,
            Some("https://gitlab.com/avatar.png".to_string())
        );
        assert_eq!(user.email, None); // GitLab doesn't return email
    }

    #[test]
    fn test_map_user_none() {
        assert!(map_user(None).is_none());
    }

    #[test]
    fn test_map_user_required_none() {
        let user = map_user_required(None);
        assert_eq!(user.id, "unknown");
        assert_eq!(user.username, "unknown");
    }

    #[test]
    fn test_map_issue() {
        let gl_issue = GitLabIssue {
            id: 1,
            iid: 42,
            title: "Test Issue".to_string(),
            description: Some("Issue body".to_string()),
            state: "opened".to_string(),
            labels: vec!["bug".to_string(), "urgent".to_string()],
            author: Some(GitLabUser {
                id: 1,
                username: "author".to_string(),
                name: None,
                avatar_url: None,
                web_url: None,
            }),
            assignees: vec![],
            web_url: "https://gitlab.com/group/project/-/issues/42".to_string(),
            created_at: "2024-01-01T00:00:00Z".to_string(),
            updated_at: "2024-01-02T00:00:00Z".to_string(),
        };

        let issue = map_issue(&gl_issue);
        assert_eq!(issue.key, "gitlab#42");
        assert_eq!(issue.title, "Test Issue");
        assert_eq!(issue.description, Some("Issue body".to_string()));
        assert_eq!(issue.state, "opened");
        assert_eq!(issue.source, "gitlab");
        assert_eq!(issue.labels, vec!["bug", "urgent"]);
        assert!(issue.author.is_some());
        assert_eq!(
            issue.url,
            Some("https://gitlab.com/group/project/-/issues/42".to_string())
        );
    }

    #[test]
    fn test_map_merge_request_states() {
        let base_mr = || GitLabMergeRequest {
            id: 1,
            iid: 10,
            title: "Test MR".to_string(),
            description: None,
            state: "opened".to_string(),
            source_branch: "feature".to_string(),
            target_branch: "main".to_string(),
            author: None,
            assignees: vec![],
            reviewers: vec![],
            labels: vec![],
            draft: false,
            work_in_progress: false,
            merged_at: None,
            web_url: "https://gitlab.com/group/project/-/merge_requests/10".to_string(),
            sha: Some("abc123".to_string()),
            diff_refs: Some(GitLabDiffRefs {
                base_sha: "base".to_string(),
                head_sha: "head".to_string(),
                start_sha: "start".to_string(),
            }),
            created_at: "2024-01-01T00:00:00Z".to_string(),
            updated_at: "2024-01-02T00:00:00Z".to_string(),
        };

        // Open MR
        let mr = map_merge_request(&base_mr());
        assert_eq!(mr.state, "opened");
        assert_eq!(mr.key, "mr#10");
        assert_eq!(mr.source, "gitlab");
        assert!(!mr.draft);

        // Draft MR
        let mut draft_mr = base_mr();
        draft_mr.draft = true;
        let mr = map_merge_request(&draft_mr);
        assert_eq!(mr.state, "draft");
        assert!(mr.draft);

        // WIP MR (legacy)
        let mut wip_mr = base_mr();
        wip_mr.work_in_progress = true;
        let mr = map_merge_request(&wip_mr);
        assert_eq!(mr.state, "draft");
        assert!(mr.draft);

        // Merged MR
        let mut merged_mr = base_mr();
        merged_mr.merged_at = Some("2024-01-03T00:00:00Z".to_string());
        merged_mr.state = "merged".to_string();
        let mr = map_merge_request(&merged_mr);
        assert_eq!(mr.state, "merged");

        // Closed MR
        let mut closed_mr = base_mr();
        closed_mr.state = "closed".to_string();
        let mr = map_merge_request(&closed_mr);
        assert_eq!(mr.state, "closed");
    }

    #[test]
    fn test_map_note() {
        let gl_note = GitLabNote {
            id: 100,
            body: "Test comment".to_string(),
            author: Some(GitLabUser {
                id: 1,
                username: "commenter".to_string(),
                name: Some("Commenter".to_string()),
                avatar_url: None,
                web_url: None,
            }),
            created_at: "2024-01-01T00:00:00Z".to_string(),
            updated_at: Some("2024-01-02T00:00:00Z".to_string()),
            system: false,
            resolvable: false,
            resolved: false,
            resolved_by: None,
            position: None,
        };

        let comment = map_note(&gl_note);
        assert_eq!(comment.id, "100");
        assert_eq!(comment.body, "Test comment");
        assert!(comment.author.is_some());
        assert_eq!(comment.author.unwrap().username, "commenter");
        assert!(comment.position.is_none());
    }

    #[test]
    fn test_map_note_with_position() {
        let gl_note = GitLabNote {
            id: 101,
            body: "Inline comment".to_string(),
            author: None,
            created_at: "2024-01-01T00:00:00Z".to_string(),
            updated_at: None,
            system: false,
            resolvable: true,
            resolved: false,
            resolved_by: None,
            position: Some(GitLabNotePosition {
                position_type: "text".to_string(),
                new_path: Some("src/main.rs".to_string()),
                old_path: Some("src/main.rs".to_string()),
                new_line: Some(42),
                old_line: None,
            }),
        };

        let comment = map_note(&gl_note);
        assert!(comment.position.is_some());
        let pos = comment.position.unwrap();
        assert_eq!(pos.file_path, "src/main.rs");
        assert_eq!(pos.line, 42);
        assert_eq!(pos.line_type, "new");
    }

    #[test]
    fn test_map_position_old_line() {
        let pos = GitLabNotePosition {
            position_type: "text".to_string(),
            new_path: Some("new.rs".to_string()),
            old_path: Some("old.rs".to_string()),
            new_line: None,
            old_line: Some(10),
        };

        let mapped = map_position(&pos).unwrap();
        assert_eq!(mapped.file_path, "old.rs");
        assert_eq!(mapped.line, 10);
        assert_eq!(mapped.line_type, "old");
    }

    #[test]
    fn test_map_position_no_lines() {
        let pos = GitLabNotePosition {
            position_type: "text".to_string(),
            new_path: Some("file.rs".to_string()),
            old_path: None,
            new_line: None,
            old_line: None,
        };

        assert!(map_position(&pos).is_none());
    }

    #[test]
    fn test_map_diff() {
        let gl_diff = GitLabDiff {
            old_path: "src/old.rs".to_string(),
            new_path: "src/new.rs".to_string(),
            new_file: false,
            renamed_file: true,
            deleted_file: false,
            diff: "@@ -1,3 +1,4 @@\n+added line\n context\n".to_string(),
        };

        let diff = map_diff(&gl_diff);
        assert_eq!(diff.file_path, "src/new.rs");
        assert_eq!(diff.old_path, Some("src/old.rs".to_string()));
        assert!(diff.renamed_file);
        assert!(!diff.new_file);
        assert!(!diff.deleted_file);
        assert!(diff.diff.contains("+added line"));
    }

    #[test]
    fn test_map_diff_new_file() {
        let gl_diff = GitLabDiff {
            old_path: "dev/null".to_string(),
            new_path: "src/new.rs".to_string(),
            new_file: true,
            renamed_file: false,
            deleted_file: false,
            diff: "+fn main() {}\n".to_string(),
        };

        let diff = map_diff(&gl_diff);
        assert_eq!(diff.file_path, "src/new.rs");
        assert!(diff.old_path.is_none()); // Not renamed, so no old_path
        assert!(diff.new_file);
    }

    #[test]
    fn test_map_discussion() {
        let gl_discussion = GitLabDiscussion {
            id: "abc123".to_string(),
            notes: vec![
                GitLabNote {
                    id: 1,
                    body: "First comment".to_string(),
                    author: None,
                    created_at: "2024-01-01T00:00:00Z".to_string(),
                    updated_at: None,
                    system: false,
                    resolvable: true,
                    resolved: true,
                    resolved_by: Some(GitLabUser {
                        id: 1,
                        username: "resolver".to_string(),
                        name: None,
                        avatar_url: None,
                        web_url: None,
                    }),
                    position: Some(GitLabNotePosition {
                        position_type: "text".to_string(),
                        new_path: Some("src/lib.rs".to_string()),
                        old_path: None,
                        new_line: Some(5),
                        old_line: None,
                    }),
                },
                GitLabNote {
                    id: 2,
                    body: "Reply".to_string(),
                    author: None,
                    created_at: "2024-01-02T00:00:00Z".to_string(),
                    updated_at: None,
                    system: false,
                    resolvable: false,
                    resolved: false,
                    resolved_by: None,
                    position: None,
                },
            ],
        };

        let discussion = map_discussion(&gl_discussion);
        assert_eq!(discussion.id, "abc123");
        assert!(discussion.resolved);
        assert!(discussion.resolved_by.is_some());
        assert_eq!(discussion.comments.len(), 2);
        assert!(discussion.position.is_some());
        assert_eq!(discussion.position.unwrap().file_path, "src/lib.rs");
    }

    #[test]
    fn test_map_discussion_filters_system_notes() {
        let gl_discussion = GitLabDiscussion {
            id: "def456".to_string(),
            notes: vec![
                GitLabNote {
                    id: 1,
                    body: "System note: assigned to @user".to_string(),
                    author: None,
                    created_at: "2024-01-01T00:00:00Z".to_string(),
                    updated_at: None,
                    system: true,
                    resolvable: false,
                    resolved: false,
                    resolved_by: None,
                    position: None,
                },
                GitLabNote {
                    id: 2,
                    body: "Actual comment".to_string(),
                    author: None,
                    created_at: "2024-01-01T00:00:00Z".to_string(),
                    updated_at: None,
                    system: false,
                    resolvable: false,
                    resolved: false,
                    resolved_by: None,
                    position: None,
                },
            ],
        };

        let discussion = map_discussion(&gl_discussion);
        assert_eq!(discussion.comments.len(), 1);
        assert_eq!(discussion.comments[0].body, "Actual comment");
    }

    // =========================================================================
    // Integration tests with httpmock
    // =========================================================================

    mod integration {
        use super::*;
        use httpmock::prelude::*;

        fn create_test_client(server: &MockServer) -> GitLabClient {
            GitLabClient::with_base_url(server.base_url(), "123", "test-token")
        }

        #[tokio::test]
        async fn test_get_issues() {
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(GET)
                    .path("/api/v4/projects/123/issues")
                    .query_param("state", "opened")
                    .query_param("per_page", "10")
                    .header("PRIVATE-TOKEN", "test-token");
                then.status(200).json_body(serde_json::json!([
                    {
                        "id": 1,
                        "iid": 42,
                        "title": "Test Issue",
                        "description": "Body",
                        "state": "opened",
                        "labels": ["bug"],
                        "author": {
                            "id": 1,
                            "username": "author",
                            "name": "Author Name"
                        },
                        "assignees": [],
                        "web_url": "https://gitlab.com/group/project/-/issues/42",
                        "created_at": "2024-01-01T00:00:00Z",
                        "updated_at": "2024-01-02T00:00:00Z"
                    }
                ]));
            });

            let client = create_test_client(&server);
            let issues = client
                .get_issues(IssueFilter {
                    state: Some("opened".to_string()),
                    limit: Some(10),
                    ..Default::default()
                })
                .await
                .unwrap();

            assert_eq!(issues.len(), 1);
            assert_eq!(issues[0].key, "gitlab#42");
            assert_eq!(issues[0].title, "Test Issue");
            assert_eq!(issues[0].state, "opened");
            assert_eq!(issues[0].labels, vec!["bug"]);
        }

        #[tokio::test]
        async fn test_get_issue() {
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(GET)
                    .path("/api/v4/projects/123/issues/42")
                    .header("PRIVATE-TOKEN", "test-token");
                then.status(200).json_body(serde_json::json!({
                    "id": 1,
                    "iid": 42,
                    "title": "Single Issue",
                    "description": "Details",
                    "state": "closed",
                    "labels": [],
                    "author": {"id": 1, "username": "author"},
                    "assignees": [{"id": 2, "username": "assignee", "name": "Assignee"}],
                    "web_url": "https://gitlab.com/group/project/-/issues/42",
                    "created_at": "2024-01-01T00:00:00Z",
                    "updated_at": "2024-01-03T00:00:00Z"
                }));
            });

            let client = create_test_client(&server);
            let issue = client.get_issue("gitlab#42").await.unwrap();

            assert_eq!(issue.key, "gitlab#42");
            assert_eq!(issue.title, "Single Issue");
            assert_eq!(issue.state, "closed");
            assert_eq!(issue.assignees.len(), 1);
            assert_eq!(issue.assignees[0].username, "assignee");
        }

        #[tokio::test]
        async fn test_create_issue() {
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(POST)
                    .path("/api/v4/projects/123/issues")
                    .header("PRIVATE-TOKEN", "test-token")
                    .body_includes("\"title\":\"New Issue\"")
                    .body_includes("\"labels\":\"bug,feature\"");
                then.status(201).json_body(serde_json::json!({
                    "id": 10,
                    "iid": 99,
                    "title": "New Issue",
                    "description": "Description",
                    "state": "opened",
                    "labels": ["bug", "feature"],
                    "author": {"id": 1, "username": "creator"},
                    "assignees": [],
                    "web_url": "https://gitlab.com/group/project/-/issues/99",
                    "created_at": "2024-02-01T00:00:00Z",
                    "updated_at": "2024-02-01T00:00:00Z"
                }));
            });

            let client = create_test_client(&server);
            let issue = client
                .create_issue(CreateIssueInput {
                    title: "New Issue".to_string(),
                    description: Some("Description".to_string()),
                    labels: vec!["bug".to_string(), "feature".to_string()],
                    assignees: vec![],
                    priority: None,
                })
                .await
                .unwrap();

            assert_eq!(issue.key, "gitlab#99");
            assert_eq!(issue.title, "New Issue");
        }

        #[tokio::test]
        async fn test_update_issue() {
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(PUT)
                    .path("/api/v4/projects/123/issues/42")
                    .header("PRIVATE-TOKEN", "test-token")
                    .body_includes("\"state_event\":\"close\"");
                then.status(200).json_body(serde_json::json!({
                    "id": 1,
                    "iid": 42,
                    "title": "Updated Issue",
                    "state": "closed",
                    "labels": [],
                    "assignees": [],
                    "web_url": "https://gitlab.com/group/project/-/issues/42",
                    "created_at": "2024-01-01T00:00:00Z",
                    "updated_at": "2024-01-05T00:00:00Z"
                }));
            });

            let client = create_test_client(&server);
            let issue = client
                .update_issue(
                    "gitlab#42",
                    UpdateIssueInput {
                        state: Some("closed".to_string()),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();

            assert_eq!(issue.state, "closed");
        }

        #[tokio::test]
        async fn test_get_merge_requests() {
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(GET)
                    .path("/api/v4/projects/123/merge_requests")
                    .header("PRIVATE-TOKEN", "test-token");
                then.status(200).json_body(serde_json::json!([
                    {
                        "id": 1,
                        "iid": 50,
                        "title": "Feature MR",
                        "description": "MR description",
                        "state": "opened",
                        "source_branch": "feature/test",
                        "target_branch": "main",
                        "author": {"id": 1, "username": "developer"},
                        "assignees": [],
                        "reviewers": [{"id": 2, "username": "reviewer"}],
                        "labels": ["review"],
                        "draft": false,
                        "work_in_progress": false,
                        "merged_at": null,
                        "web_url": "https://gitlab.com/group/project/-/merge_requests/50",
                        "sha": "abc123",
                        "diff_refs": {
                            "base_sha": "base",
                            "head_sha": "head",
                            "start_sha": "start"
                        },
                        "created_at": "2024-01-01T00:00:00Z",
                        "updated_at": "2024-01-02T00:00:00Z"
                    }
                ]));
            });

            let client = create_test_client(&server);
            let mrs = client
                .get_merge_requests(MrFilter::default())
                .await
                .unwrap();

            assert_eq!(mrs.len(), 1);
            assert_eq!(mrs[0].key, "mr#50");
            assert_eq!(mrs[0].title, "Feature MR");
            assert_eq!(mrs[0].state, "opened");
            assert_eq!(mrs[0].source_branch, "feature/test");
            assert_eq!(mrs[0].reviewers.len(), 1);
        }

        #[tokio::test]
        async fn test_get_discussions() {
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(GET)
                    .path("/api/v4/projects/123/merge_requests/50/discussions")
                    .header("PRIVATE-TOKEN", "test-token");
                then.status(200).json_body(serde_json::json!([
                    {
                        "id": "disc-1",
                        "notes": [
                            {
                                "id": 100,
                                "body": "Please fix this",
                                "author": {"id": 1, "username": "reviewer"},
                                "created_at": "2024-01-01T00:00:00Z",
                                "system": false,
                                "resolvable": true,
                                "resolved": false,
                                "position": {
                                    "position_type": "text",
                                    "new_path": "src/lib.rs",
                                    "old_path": "src/lib.rs",
                                    "new_line": 42,
                                    "old_line": null
                                }
                            },
                            {
                                "id": 101,
                                "body": "Fixed!",
                                "author": {"id": 2, "username": "developer"},
                                "created_at": "2024-01-02T00:00:00Z",
                                "system": false,
                                "resolvable": false,
                                "resolved": false
                            }
                        ]
                    },
                    {
                        "id": "disc-system",
                        "notes": [
                            {
                                "id": 200,
                                "body": "merged",
                                "created_at": "2024-01-03T00:00:00Z",
                                "system": true,
                                "resolvable": false,
                                "resolved": false
                            }
                        ]
                    }
                ]));
            });

            let client = create_test_client(&server);
            let discussions = client.get_discussions("mr#50").await.unwrap();

            // System-only discussion should be filtered out
            assert_eq!(discussions.len(), 1);
            assert_eq!(discussions[0].id, "disc-1");
            assert_eq!(discussions[0].comments.len(), 2);
            assert!(!discussions[0].resolved);
            assert!(discussions[0].position.is_some());
        }

        #[tokio::test]
        async fn test_get_diffs() {
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(GET)
                    .path("/api/v4/projects/123/merge_requests/50/changes")
                    .header("PRIVATE-TOKEN", "test-token");
                then.status(200).json_body(serde_json::json!({
                    "changes": [
                        {
                            "old_path": "src/main.rs",
                            "new_path": "src/main.rs",
                            "new_file": false,
                            "renamed_file": false,
                            "deleted_file": false,
                            "diff": "@@ -1,3 +1,4 @@\n+use tracing;\n fn main() {\n }\n"
                        },
                        {
                            "old_path": "/dev/null",
                            "new_path": "src/new_file.rs",
                            "new_file": true,
                            "renamed_file": false,
                            "deleted_file": false,
                            "diff": "+pub fn new_fn() {}\n"
                        }
                    ]
                }));
            });

            let client = create_test_client(&server);
            let diffs = client.get_diffs("mr#50").await.unwrap();

            assert_eq!(diffs.len(), 2);
            assert_eq!(diffs[0].file_path, "src/main.rs");
            assert!(!diffs[0].new_file);
            assert!(diffs[0].diff.contains("+use tracing"));
            assert_eq!(diffs[1].file_path, "src/new_file.rs");
            assert!(diffs[1].new_file);
        }

        #[tokio::test]
        async fn test_add_mr_comment_general() {
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(POST)
                    .path("/api/v4/projects/123/merge_requests/50/notes")
                    .header("PRIVATE-TOKEN", "test-token")
                    .body_includes("\"body\":\"General comment\"");
                then.status(201).json_body(serde_json::json!({
                    "id": 300,
                    "body": "General comment",
                    "author": {"id": 1, "username": "commenter"},
                    "created_at": "2024-01-01T00:00:00Z",
                    "system": false,
                    "resolvable": false,
                    "resolved": false
                }));
            });

            let client = create_test_client(&server);
            let comment = MergeRequestProvider::add_comment(
                &client,
                "mr#50",
                CreateCommentInput {
                    body: "General comment".to_string(),
                    position: None,
                    discussion_id: None,
                },
            )
            .await
            .unwrap();

            assert_eq!(comment.id, "300");
            assert_eq!(comment.body, "General comment");
        }

        #[tokio::test]
        async fn test_add_mr_comment_inline() {
            let server = MockServer::start();

            // Mock fetching MR to get diff_refs
            server.mock(|when, then| {
                when.method(GET)
                    .path("/api/v4/projects/123/merge_requests/50");
                then.status(200).json_body(serde_json::json!({
                    "id": 1,
                    "iid": 50,
                    "title": "Test MR",
                    "state": "opened",
                    "source_branch": "feature",
                    "target_branch": "main",
                    "web_url": "https://gitlab.com/group/project/-/merge_requests/50",
                    "sha": "abc123",
                    "diff_refs": {
                        "base_sha": "base_sha_val",
                        "head_sha": "head_sha_val",
                        "start_sha": "start_sha_val"
                    },
                    "created_at": "2024-01-01T00:00:00Z",
                    "updated_at": "2024-01-02T00:00:00Z"
                }));
            });

            // Mock creating discussion
            server.mock(|when, then| {
                when.method(POST)
                    .path("/api/v4/projects/123/merge_requests/50/discussions")
                    .body_includes("\"position\"")
                    .body_includes("\"base_sha\":\"base_sha_val\"");
                then.status(201).json_body(serde_json::json!({
                    "id": "new-disc",
                    "notes": [{
                        "id": 400,
                        "body": "Inline comment",
                        "author": {"id": 1, "username": "reviewer"},
                        "created_at": "2024-01-01T00:00:00Z",
                        "system": false,
                        "resolvable": true,
                        "resolved": false,
                        "position": {
                            "position_type": "text",
                            "new_path": "src/lib.rs",
                            "new_line": 10
                        }
                    }]
                }));
            });

            let client = create_test_client(&server);
            let comment = MergeRequestProvider::add_comment(
                &client,
                "mr#50",
                CreateCommentInput {
                    body: "Inline comment".to_string(),
                    position: Some(CodePosition {
                        file_path: "src/lib.rs".to_string(),
                        line: 10,
                        line_type: "new".to_string(),
                        commit_sha: None,
                    }),
                    discussion_id: None,
                },
            )
            .await
            .unwrap();

            assert_eq!(comment.id, "400");
            assert_eq!(comment.body, "Inline comment");
            assert!(comment.position.is_some());
        }

        #[tokio::test]
        async fn test_get_current_user() {
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(GET)
                    .path("/api/v4/user")
                    .header("PRIVATE-TOKEN", "test-token");
                then.status(200).json_body(serde_json::json!({
                    "id": 42,
                    "username": "current_user",
                    "name": "Current User",
                    "avatar_url": "https://gitlab.com/avatar.png",
                    "web_url": "https://gitlab.com/current_user"
                }));
            });

            let client = create_test_client(&server);
            let user = client.get_current_user().await.unwrap();

            assert_eq!(user.id, "42");
            assert_eq!(user.username, "current_user");
            assert_eq!(user.name, Some("Current User".to_string()));
        }

        #[tokio::test]
        async fn test_api_error_handling() {
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(GET).path("/api/v4/projects/123/issues/999");
                then.status(404).body("{\"message\":\"404 Not Found\"}");
            });

            let client = create_test_client(&server);
            let result = client.get_issue("gitlab#999").await;

            assert!(result.is_err());
            assert!(matches!(result.unwrap_err(), Error::NotFound(_)));
        }

        #[tokio::test]
        async fn test_unauthorized_error() {
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(GET).path("/api/v4/user");
                then.status(401).body("{\"message\":\"401 Unauthorized\"}");
            });

            let client = create_test_client(&server);
            let result = client.get_current_user().await;

            assert!(result.is_err());
            assert!(matches!(result.unwrap_err(), Error::Unauthorized(_)));
        }
    }
}
