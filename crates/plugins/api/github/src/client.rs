//! GitHub API client implementation.

use async_trait::async_trait;
use devboy_core::{
    CodePosition, Comment, CreateCommentInput, CreateIssueInput, Discussion, Error, FileDiff,
    Issue, IssueFilter, IssueProvider, MergeRequest, MergeRequestProvider, MrFilter, Provider,
    Result, UpdateIssueInput, User,
};
use tracing::{debug, warn};

use crate::types::{
    CreateCommentRequest, CreateIssueRequest, CreateReviewCommentRequest, GitHubComment,
    GitHubFile, GitHubIssue, GitHubLabel, GitHubPullRequest, GitHubReview, GitHubReviewComment,
    GitHubUser, UpdateIssueRequest,
};
use crate::DEFAULT_GITHUB_URL;

/// GitHub API client.
pub struct GitHubClient {
    base_url: String,
    owner: String,
    repo: String,
    token: String,
    client: reqwest::Client,
}

impl GitHubClient {
    /// Create a new GitHub client.
    pub fn new(
        owner: impl Into<String>,
        repo: impl Into<String>,
        token: impl Into<String>,
    ) -> Self {
        Self::with_base_url(DEFAULT_GITHUB_URL, owner, repo, token)
    }

    /// Create a new GitHub client with a custom base URL.
    pub fn with_base_url(
        base_url: impl Into<String>,
        owner: impl Into<String>,
        repo: impl Into<String>,
        token: impl Into<String>,
    ) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            owner: owner.into(),
            repo: repo.into(),
            token: token.into(),
            client: reqwest::Client::builder()
                .user_agent("devboy-tools")
                .build()
                .expect("Failed to create HTTP client"),
        }
    }

    /// Build request with common headers.
    fn request(&self, method: reqwest::Method, url: &str) -> reqwest::RequestBuilder {
        self.client
            .request(method, url)
            .header("Authorization", format!("Bearer {}", self.token))
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
    }

    /// Make an authenticated GET request.
    async fn get<T: serde::de::DeserializeOwned>(&self, url: &str) -> Result<T> {
        debug!(url = url, "GitHub GET request");

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
        debug!(url = url, "GitHub POST request");

        let response = self
            .request(reqwest::Method::POST, url)
            .json(body)
            .send()
            .await
            .map_err(|e| Error::Http(e.to_string()))?;

        self.handle_response(response).await
    }

    /// Make an authenticated PATCH request.
    async fn patch<T: serde::de::DeserializeOwned, B: serde::Serialize>(
        &self,
        url: &str,
        body: &B,
    ) -> Result<T> {
        debug!(url = url, "GitHub PATCH request");

        let response = self
            .request(reqwest::Method::PATCH, url)
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
                "GitHub API error response"
            );
            return Err(Error::from_status(status_code, message));
        }

        response
            .json()
            .await
            .map_err(|e| Error::InvalidData(format!("Failed to parse response: {}", e)))
    }

    /// Build repo API URL.
    fn repo_url(&self, endpoint: &str) -> String {
        format!(
            "{}/repos/{}/{}{}",
            self.base_url, self.owner, self.repo, endpoint
        )
    }
}

// =============================================================================
// Mapping functions: GitHub types -> Unified types
// =============================================================================

fn map_user(gh_user: Option<&GitHubUser>) -> Option<User> {
    gh_user.map(|u| User {
        id: u.id.to_string(),
        username: u.login.clone(),
        name: u.name.clone(),
        email: u.email.clone(),
        avatar_url: u.avatar_url.clone(),
    })
}

fn map_user_required(gh_user: Option<&GitHubUser>) -> User {
    map_user(gh_user).unwrap_or_else(|| User {
        id: "unknown".to_string(),
        username: "unknown".to_string(),
        name: Some("Unknown".to_string()),
        ..Default::default()
    })
}

fn map_labels(labels: &[GitHubLabel]) -> Vec<String> {
    labels.iter().map(|l| l.name.clone()).collect()
}

fn map_issue(gh_issue: &GitHubIssue) -> Issue {
    Issue {
        key: format!("gh#{}", gh_issue.number),
        title: gh_issue.title.clone(),
        description: gh_issue.body.clone(),
        state: gh_issue.state.clone(),
        source: "github".to_string(),
        priority: None, // GitHub doesn't have built-in priority
        labels: map_labels(&gh_issue.labels),
        author: map_user(gh_issue.user.as_ref()),
        assignees: gh_issue.assignees.iter().map(|u| map_user_required(Some(u))).collect(),
        url: Some(gh_issue.html_url.clone()),
        created_at: Some(gh_issue.created_at.clone()),
        updated_at: Some(gh_issue.updated_at.clone()),
    }
}

fn map_pull_request(gh_pr: &GitHubPullRequest) -> MergeRequest {
    // Determine state
    let state = if gh_pr.merged || gh_pr.merged_at.is_some() {
        "merged".to_string()
    } else if gh_pr.state == "closed" {
        "closed".to_string()
    } else if gh_pr.draft {
        "draft".to_string()
    } else {
        "open".to_string()
    };

    MergeRequest {
        key: format!("pr#{}", gh_pr.number),
        title: gh_pr.title.clone(),
        description: gh_pr.body.clone(),
        state,
        source: "github".to_string(),
        source_branch: gh_pr.head.ref_name.clone(),
        target_branch: gh_pr.base.ref_name.clone(),
        author: map_user(gh_pr.user.as_ref()),
        assignees: gh_pr
            .assignees
            .iter()
            .map(|u| map_user_required(Some(u)))
            .collect(),
        reviewers: gh_pr
            .requested_reviewers
            .iter()
            .map(|u| map_user_required(Some(u)))
            .collect(),
        labels: map_labels(&gh_pr.labels),
        draft: gh_pr.draft,
        url: Some(gh_pr.html_url.clone()),
        created_at: Some(gh_pr.created_at.clone()),
        updated_at: Some(gh_pr.updated_at.clone()),
    }
}

fn map_comment(gh_comment: &GitHubComment) -> Comment {
    Comment {
        id: gh_comment.id.to_string(),
        body: gh_comment.body.clone(),
        author: map_user(gh_comment.user.as_ref()),
        created_at: Some(gh_comment.created_at.clone()),
        updated_at: gh_comment.updated_at.clone(),
        position: None,
    }
}

fn map_review_comment(gh_comment: &GitHubReviewComment) -> Comment {
    let position = gh_comment.line.or(gh_comment.original_line).map(|line| CodePosition {
        file_path: gh_comment.path.clone(),
        line,
        line_type: gh_comment
            .side
            .as_ref()
            .map(|s| if s == "LEFT" { "old" } else { "new" })
            .unwrap_or("new")
            .to_string(),
        commit_sha: gh_comment
            .commit_id
            .clone()
            .or_else(|| gh_comment.original_commit_id.clone()),
    });

    Comment {
        id: gh_comment.id.to_string(),
        body: gh_comment.body.clone(),
        author: map_user(gh_comment.user.as_ref()),
        created_at: Some(gh_comment.created_at.clone()),
        updated_at: gh_comment.updated_at.clone(),
        position,
    }
}

fn map_file(gh_file: &GitHubFile) -> FileDiff {
    FileDiff {
        file_path: gh_file.filename.clone(),
        old_path: gh_file.previous_filename.clone(),
        new_file: gh_file.status == "added",
        deleted_file: gh_file.status == "removed",
        renamed_file: gh_file.status == "renamed",
        diff: gh_file.patch.clone().unwrap_or_default(),
        additions: Some(gh_file.additions),
        deletions: Some(gh_file.deletions),
    }
}

// =============================================================================
// Trait implementations
// =============================================================================

#[async_trait]
impl IssueProvider for GitHubClient {
    async fn get_issues(&self, filter: IssueFilter) -> Result<Vec<Issue>> {
        let mut url = self.repo_url("/issues");
        let mut params = vec![];

        // Map state
        if let Some(state) = &filter.state {
            let gh_state = match state.as_str() {
                "opened" | "open" => "open",
                "closed" => "closed",
                "all" => "all",
                _ => "open",
            };
            params.push(format!("state={}", gh_state));
        }

        if let Some(labels) = &filter.labels {
            if !labels.is_empty() {
                params.push(format!("labels={}", labels.join(",")));
            }
        }

        if let Some(assignee) = &filter.assignee {
            params.push(format!("assignee={}", assignee));
        }

        if let Some(limit) = filter.limit {
            params.push(format!("per_page={}", limit.min(100)));
        }

        if let Some(offset) = filter.offset {
            // GitHub uses page-based pagination
            let per_page = filter.limit.unwrap_or(30);
            let page = (offset / per_page) + 1;
            params.push(format!("page={}", page));
        }

        if let Some(sort_by) = &filter.sort_by {
            let gh_sort = match sort_by.as_str() {
                "created_at" | "created" => "created",
                "updated_at" | "updated" => "updated",
                _ => "updated",
            };
            params.push(format!("sort={}", gh_sort));
        }

        if let Some(order) = &filter.sort_order {
            params.push(format!("direction={}", order));
        }

        if !params.is_empty() {
            url.push_str(&format!("?{}", params.join("&")));
        }

        let gh_issues: Vec<GitHubIssue> = self.get(&url).await?;

        // Filter out pull requests (GitHub returns PRs in /issues endpoint)
        let issues: Vec<Issue> = gh_issues
            .iter()
            .filter(|i| i.pull_request.is_none())
            .map(map_issue)
            .collect();

        Ok(issues)
    }

    async fn get_issue(&self, key: &str) -> Result<Issue> {
        let number = parse_issue_key(key)?;
        let url = self.repo_url(&format!("/issues/{}", number));
        let gh_issue: GitHubIssue = self.get(&url).await?;

        // Make sure it's not a PR
        if gh_issue.pull_request.is_some() {
            return Err(Error::InvalidData(format!(
                "{} is a pull request, not an issue",
                key
            )));
        }

        Ok(map_issue(&gh_issue))
    }

    async fn create_issue(&self, input: CreateIssueInput) -> Result<Issue> {
        let url = self.repo_url("/issues");
        let request = CreateIssueRequest {
            title: input.title,
            body: input.description,
            labels: input.labels,
            assignees: input.assignees,
        };

        let gh_issue: GitHubIssue = self.post(&url, &request).await?;
        Ok(map_issue(&gh_issue))
    }

    async fn update_issue(&self, key: &str, input: UpdateIssueInput) -> Result<Issue> {
        let number = parse_issue_key(key)?;
        let url = self.repo_url(&format!("/issues/{}", number));

        // Map state
        let state = input.state.map(|s| match s.as_str() {
            "opened" | "open" => "open".to_string(),
            "closed" => "closed".to_string(),
            _ => s,
        });

        let request = UpdateIssueRequest {
            title: input.title,
            body: input.description,
            state,
            labels: input.labels,
            assignees: input.assignees,
        };

        let gh_issue: GitHubIssue = self.patch(&url, &request).await?;
        Ok(map_issue(&gh_issue))
    }

    async fn get_comments(&self, issue_key: &str) -> Result<Vec<Comment>> {
        let number = parse_issue_key(issue_key)?;
        let url = self.repo_url(&format!("/issues/{}/comments", number));
        let gh_comments: Vec<GitHubComment> = self.get(&url).await?;
        Ok(gh_comments.iter().map(map_comment).collect())
    }

    async fn add_comment(&self, issue_key: &str, body: &str) -> Result<Comment> {
        let number = parse_issue_key(issue_key)?;
        let url = self.repo_url(&format!("/issues/{}/comments", number));
        let request = CreateCommentRequest {
            body: body.to_string(),
        };

        let gh_comment: GitHubComment = self.post(&url, &request).await?;
        Ok(map_comment(&gh_comment))
    }

    fn provider_name(&self) -> &'static str {
        "github"
    }
}

#[async_trait]
impl MergeRequestProvider for GitHubClient {
    async fn get_merge_requests(&self, filter: MrFilter) -> Result<Vec<MergeRequest>> {
        let mut url = self.repo_url("/pulls");
        let mut params = vec![];

        // Map state
        if let Some(state) = &filter.state {
            let gh_state = match state.as_str() {
                "opened" | "open" => "open",
                "closed" => "closed",
                "merged" => "closed", // GitHub doesn't have merged state in filter
                "all" => "all",
                _ => "open",
            };
            params.push(format!("state={}", gh_state));
        }

        if let Some(source_branch) = &filter.source_branch {
            params.push(format!("head={}", source_branch));
        }

        if let Some(target_branch) = &filter.target_branch {
            params.push(format!("base={}", target_branch));
        }

        if let Some(limit) = filter.limit {
            params.push(format!("per_page={}", limit.min(100)));
        }

        params.push("sort=updated".to_string());
        params.push("direction=desc".to_string());

        if !params.is_empty() {
            url.push_str(&format!("?{}", params.join("&")));
        }

        let gh_prs: Vec<GitHubPullRequest> = self.get(&url).await?;

        let mut prs: Vec<MergeRequest> = gh_prs.iter().map(map_pull_request).collect();

        // Filter by merged state if requested
        if filter.state.as_deref() == Some("merged") {
            prs.retain(|pr| pr.state == "merged");
        }

        Ok(prs)
    }

    async fn get_merge_request(&self, key: &str) -> Result<MergeRequest> {
        let number = parse_pr_key(key)?;
        let url = self.repo_url(&format!("/pulls/{}", number));
        let gh_pr: GitHubPullRequest = self.get(&url).await?;
        Ok(map_pull_request(&gh_pr))
    }

    async fn get_discussions(&self, mr_key: &str) -> Result<Vec<Discussion>> {
        let number = parse_pr_key(mr_key)?;

        // Fetch reviews, review comments, and general comments
        let reviews_url = self.repo_url(&format!("/pulls/{}/reviews", number));
        let review_comments_url = self.repo_url(&format!("/pulls/{}/comments", number));
        let issue_comments_url = self.repo_url(&format!("/issues/{}/comments", number));

        let reviews: Vec<GitHubReview> = self.get(&reviews_url).await?;
        let review_comments: Vec<GitHubReviewComment> = self.get(&review_comments_url).await?;
        let issue_comments: Vec<GitHubComment> = self.get(&issue_comments_url).await?;

        let mut discussions = Vec::new();

        // Group review comments by thread
        let mut comment_threads: std::collections::HashMap<u64, Vec<&GitHubReviewComment>> =
            std::collections::HashMap::new();

        for comment in &review_comments {
            let thread_id = comment.in_reply_to_id.unwrap_or(comment.id);
            comment_threads
                .entry(thread_id)
                .or_default()
                .push(comment);
        }

        // Create discussions from threads
        for (thread_id, comments) in comment_threads {
            let mapped_comments: Vec<Comment> = comments.iter().map(|c| map_review_comment(c)).collect();
            let position = mapped_comments.first().and_then(|c| c.position.clone());

            discussions.push(Discussion {
                id: format!("thread-{}", thread_id),
                resolved: false, // GitHub doesn't have resolved state for review comments
                resolved_by: None,
                comments: mapped_comments,
                position,
            });
        }

        // Add reviews as discussions
        for review in &reviews {
            let mut comments = Vec::new();
            if let Some(body) = &review.body {
                if !body.is_empty() {
                    comments.push(Comment {
                        id: review.id.to_string(),
                        body: body.clone(),
                        author: map_user(review.user.as_ref()),
                        created_at: review.submitted_at.clone(),
                        updated_at: None,
                        position: None,
                    });
                }
            }

            if !comments.is_empty() || !review.state.is_empty() {
                discussions.push(Discussion {
                    id: format!("review-{}", review.id),
                    resolved: false,
                    resolved_by: None,
                    comments,
                    position: None,
                });
            }
        }

        // Add general PR comments
        for comment in &issue_comments {
            discussions.push(Discussion {
                id: format!("comment-{}", comment.id),
                resolved: false,
                resolved_by: None,
                comments: vec![map_comment(comment)],
                position: None,
            });
        }

        Ok(discussions)
    }

    async fn get_diffs(&self, mr_key: &str) -> Result<Vec<FileDiff>> {
        let number = parse_pr_key(mr_key)?;
        let url = self.repo_url(&format!("/pulls/{}/files", number));
        let gh_files: Vec<GitHubFile> = self.get(&url).await?;
        Ok(gh_files.iter().map(map_file).collect())
    }

    async fn add_comment(&self, mr_key: &str, input: CreateCommentInput) -> Result<Comment> {
        let number = parse_pr_key(mr_key)?;

        // If position is provided, create a review comment
        if let Some(position) = &input.position {
            let url = self.repo_url(&format!("/pulls/{}/comments", number));

            let commit_sha = position
                .commit_sha
                .clone()
                .ok_or_else(|| Error::InvalidData("commit_sha is required for code comments".to_string()))?;

            let request = CreateReviewCommentRequest {
                body: input.body,
                commit_id: commit_sha,
                path: position.file_path.clone(),
                line: Some(position.line),
                side: Some(if position.line_type == "old" {
                    "LEFT".to_string()
                } else {
                    "RIGHT".to_string()
                }),
                in_reply_to: input.discussion_id.and_then(|id| id.parse().ok()),
            };

            let gh_comment: GitHubReviewComment = self.post(&url, &request).await?;
            return Ok(map_review_comment(&gh_comment));
        }

        // Otherwise create a general comment
        let url = self.repo_url(&format!("/issues/{}/comments", number));
        let request = CreateCommentRequest { body: input.body };

        let gh_comment: GitHubComment = self.post(&url, &request).await?;
        Ok(map_comment(&gh_comment))
    }

    fn provider_name(&self) -> &'static str {
        "github"
    }
}

#[async_trait]
impl Provider for GitHubClient {
    async fn get_current_user(&self) -> Result<User> {
        let url = format!("{}/user", self.base_url);
        let gh_user: GitHubUser = self.get(&url).await?;
        Ok(map_user_required(Some(&gh_user)))
    }
}

// =============================================================================
// Helper functions
// =============================================================================

/// Parse issue key like "gh#123" to get issue number.
fn parse_issue_key(key: &str) -> Result<u64> {
    key.strip_prefix("gh#")
        .and_then(|s| s.parse::<u64>().ok())
        .ok_or_else(|| Error::InvalidData(format!("Invalid issue key: {}", key)))
}

/// Parse PR key like "pr#123" to get PR number.
fn parse_pr_key(key: &str) -> Result<u64> {
    key.strip_prefix("pr#")
        .and_then(|s| s.parse::<u64>().ok())
        .ok_or_else(|| Error::InvalidData(format!("Invalid PR key: {}", key)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::GitHubBranchRef;

    #[test]
    fn test_parse_issue_key() {
        assert_eq!(parse_issue_key("gh#123").unwrap(), 123);
        assert_eq!(parse_issue_key("gh#1").unwrap(), 1);
        assert!(parse_issue_key("pr#123").is_err());
        assert!(parse_issue_key("123").is_err());
        assert!(parse_issue_key("gh#").is_err());
    }

    #[test]
    fn test_parse_pr_key() {
        assert_eq!(parse_pr_key("pr#456").unwrap(), 456);
        assert_eq!(parse_pr_key("pr#1").unwrap(), 1);
        assert!(parse_pr_key("gh#123").is_err());
        assert!(parse_pr_key("456").is_err());
    }

    #[test]
    fn test_map_user() {
        let gh_user = GitHubUser {
            id: 123,
            login: "testuser".to_string(),
            name: Some("Test User".to_string()),
            email: Some("test@example.com".to_string()),
            avatar_url: Some("https://example.com/avatar.png".to_string()),
        };

        let user = map_user(Some(&gh_user)).unwrap();
        assert_eq!(user.id, "123");
        assert_eq!(user.username, "testuser");
        assert_eq!(user.name, Some("Test User".to_string()));
        assert_eq!(user.email, Some("test@example.com".to_string()));
    }

    #[test]
    fn test_map_issue() {
        let gh_issue = GitHubIssue {
            id: 1,
            number: 42,
            title: "Test Issue".to_string(),
            body: Some("Issue body".to_string()),
            state: "open".to_string(),
            html_url: "https://github.com/test/repo/issues/42".to_string(),
            user: Some(GitHubUser {
                id: 1,
                login: "author".to_string(),
                name: None,
                email: None,
                avatar_url: None,
            }),
            assignees: vec![],
            labels: vec![GitHubLabel {
                id: 1,
                name: "bug".to_string(),
                color: None,
                description: None,
            }],
            created_at: "2024-01-01T00:00:00Z".to_string(),
            updated_at: "2024-01-02T00:00:00Z".to_string(),
            closed_at: None,
            pull_request: None,
        };

        let issue = map_issue(&gh_issue);
        assert_eq!(issue.key, "gh#42");
        assert_eq!(issue.title, "Test Issue");
        assert_eq!(issue.state, "open");
        assert_eq!(issue.source, "github");
        assert_eq!(issue.labels, vec!["bug"]);
    }

    #[test]
    fn test_map_pull_request_states() {
        let base_pr = || GitHubPullRequest {
            id: 1,
            number: 10,
            title: "Test PR".to_string(),
            body: None,
            state: "open".to_string(),
            html_url: "https://github.com/test/repo/pull/10".to_string(),
            draft: false,
            merged: false,
            merged_at: None,
            user: None,
            assignees: vec![],
            requested_reviewers: vec![],
            labels: vec![],
            head: GitHubBranchRef {
                ref_name: "feature".to_string(),
                sha: "abc123".to_string(),
            },
            base: GitHubBranchRef {
                ref_name: "main".to_string(),
                sha: "def456".to_string(),
            },
            created_at: "2024-01-01T00:00:00Z".to_string(),
            updated_at: "2024-01-02T00:00:00Z".to_string(),
        };

        // Open PR
        let pr = map_pull_request(&base_pr());
        assert_eq!(pr.state, "open");

        // Draft PR
        let mut draft_pr = base_pr();
        draft_pr.draft = true;
        let pr = map_pull_request(&draft_pr);
        assert_eq!(pr.state, "draft");

        // Merged PR
        let mut merged_pr = base_pr();
        merged_pr.merged = true;
        let pr = map_pull_request(&merged_pr);
        assert_eq!(pr.state, "merged");

        // Closed PR
        let mut closed_pr = base_pr();
        closed_pr.state = "closed".to_string();
        let pr = map_pull_request(&closed_pr);
        assert_eq!(pr.state, "closed");
    }
}
