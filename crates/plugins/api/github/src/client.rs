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
        assignees: gh_issue
            .assignees
            .iter()
            .map(|u| map_user_required(Some(u)))
            .collect(),
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
    let position = gh_comment
        .line
        .or(gh_comment.original_line)
        .map(|line| CodePosition {
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
            comment_threads.entry(thread_id).or_default().push(comment);
        }

        // Create discussions from threads
        for (thread_id, comments) in comment_threads {
            let mapped_comments: Vec<Comment> =
                comments.iter().map(|c| map_review_comment(c)).collect();
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

        // First verify that this is actually a PR, not an issue
        let pr_url = self.repo_url(&format!("/pulls/{}", number));
        let pr_result: Result<GitHubPullRequest> = self.get(&pr_url).await;

        if let Err(Error::Http(status)) = &pr_result {
            if status.contains("404") {
                return Err(Error::InvalidData(format!(
                    "{} is not a valid pull request (it may be an issue)",
                    mr_key
                )));
            }
        }

        // Propagate other errors and save PR for later use
        let pr: GitHubPullRequest = pr_result?;

        // If position is provided, create a review comment
        if let Some(position) = &input.position {
            let url = self.repo_url(&format!("/pulls/{}/comments", number));

            // If commit_sha is not provided, use the PR head commit
            let commit_sha = if let Some(sha) = &position.commit_sha {
                sha.clone()
            } else {
                // Use the already fetched PR head commit SHA
                pr.head.sha
            };

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

        // Otherwise create a general comment using PR endpoint
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
    fn test_map_user_none() {
        assert!(map_user(None).is_none());
    }

    #[test]
    fn test_map_user_required_with_user() {
        let gh_user = GitHubUser {
            id: 1,
            login: "user1".to_string(),
            name: Some("User One".to_string()),
            email: None,
            avatar_url: None,
        };
        let user = map_user_required(Some(&gh_user));
        assert_eq!(user.username, "user1");
    }

    #[test]
    fn test_map_user_required_without_user() {
        let user = map_user_required(None);
        assert_eq!(user.id, "unknown");
        assert_eq!(user.username, "unknown");
        assert_eq!(user.name, Some("Unknown".to_string()));
    }

    #[test]
    fn test_map_labels() {
        let labels = vec![
            GitHubLabel {
                id: 1,
                name: "bug".to_string(),
                color: None,
                description: None,
            },
            GitHubLabel {
                id: 2,
                name: "feature".to_string(),
                color: Some("00ff00".to_string()),
                description: Some("Feature request".to_string()),
            },
        ];
        let result = map_labels(&labels);
        assert_eq!(result, vec!["bug", "feature"]);
    }

    #[test]
    fn test_map_labels_empty() {
        let result = map_labels(&[]);
        assert!(result.is_empty());
    }

    #[test]
    fn test_map_comment() {
        let gh_comment = GitHubComment {
            id: 42,
            body: "Nice work!".to_string(),
            user: Some(GitHubUser {
                id: 1,
                login: "reviewer".to_string(),
                name: None,
                email: None,
                avatar_url: None,
            }),
            created_at: "2024-01-15T10:00:00Z".to_string(),
            updated_at: Some("2024-01-15T12:00:00Z".to_string()),
        };

        let comment = map_comment(&gh_comment);
        assert_eq!(comment.id, "42");
        assert_eq!(comment.body, "Nice work!");
        assert!(comment.author.is_some());
        assert_eq!(comment.author.unwrap().username, "reviewer");
        assert_eq!(comment.created_at, Some("2024-01-15T10:00:00Z".to_string()));
        assert_eq!(comment.updated_at, Some("2024-01-15T12:00:00Z".to_string()));
        assert!(comment.position.is_none());
    }

    #[test]
    fn test_map_review_comment_with_line() {
        let gh_comment = GitHubReviewComment {
            id: 100,
            body: "Fix this".to_string(),
            user: Some(GitHubUser {
                id: 1,
                login: "reviewer".to_string(),
                name: None,
                email: None,
                avatar_url: None,
            }),
            created_at: "2024-01-15T10:00:00Z".to_string(),
            updated_at: None,
            path: "src/main.rs".to_string(),
            line: Some(42),
            original_line: None,
            position: None,
            side: Some("RIGHT".to_string()),
            diff_hunk: None,
            commit_id: Some("abc123".to_string()),
            original_commit_id: None,
            in_reply_to_id: None,
        };

        let comment = map_review_comment(&gh_comment);
        assert_eq!(comment.id, "100");
        assert_eq!(comment.body, "Fix this");
        let pos = comment.position.unwrap();
        assert_eq!(pos.file_path, "src/main.rs");
        assert_eq!(pos.line, 42);
        assert_eq!(pos.line_type, "new");
        assert_eq!(pos.commit_sha, Some("abc123".to_string()));
    }

    #[test]
    fn test_map_review_comment_with_left_side() {
        let gh_comment = GitHubReviewComment {
            id: 101,
            body: "Old code".to_string(),
            user: None,
            created_at: "2024-01-15T10:00:00Z".to_string(),
            updated_at: None,
            path: "src/lib.rs".to_string(),
            line: Some(10),
            original_line: None,
            position: None,
            side: Some("LEFT".to_string()),
            diff_hunk: None,
            commit_id: None,
            original_commit_id: Some("def456".to_string()),
            in_reply_to_id: None,
        };

        let comment = map_review_comment(&gh_comment);
        let pos = comment.position.unwrap();
        assert_eq!(pos.line_type, "old");
        assert_eq!(pos.commit_sha, Some("def456".to_string()));
    }

    #[test]
    fn test_map_review_comment_with_original_line_fallback() {
        let gh_comment = GitHubReviewComment {
            id: 102,
            body: "Outdated".to_string(),
            user: None,
            created_at: "2024-01-15T10:00:00Z".to_string(),
            updated_at: None,
            path: "src/lib.rs".to_string(),
            line: None,
            original_line: Some(5),
            position: None,
            side: None,
            diff_hunk: None,
            commit_id: None,
            original_commit_id: None,
            in_reply_to_id: None,
        };

        let comment = map_review_comment(&gh_comment);
        let pos = comment.position.unwrap();
        assert_eq!(pos.line, 5);
        assert_eq!(pos.line_type, "new"); // default when no side
    }

    #[test]
    fn test_map_review_comment_without_line() {
        let gh_comment = GitHubReviewComment {
            id: 103,
            body: "General".to_string(),
            user: None,
            created_at: "2024-01-15T10:00:00Z".to_string(),
            updated_at: None,
            path: "src/lib.rs".to_string(),
            line: None,
            original_line: None,
            position: None,
            side: None,
            diff_hunk: None,
            commit_id: None,
            original_commit_id: None,
            in_reply_to_id: None,
        };

        let comment = map_review_comment(&gh_comment);
        assert!(comment.position.is_none());
    }

    #[test]
    fn test_map_file() {
        let gh_file = GitHubFile {
            sha: "abc123".to_string(),
            filename: "src/main.rs".to_string(),
            status: "modified".to_string(),
            additions: 10,
            deletions: 3,
            changes: 13,
            patch: Some("@@ -1,3 +1,10 @@\n+new line".to_string()),
            previous_filename: None,
        };

        let diff = map_file(&gh_file);
        assert_eq!(diff.file_path, "src/main.rs");
        assert!(!diff.new_file);
        assert!(!diff.deleted_file);
        assert!(!diff.renamed_file);
        assert_eq!(diff.additions, Some(10));
        assert_eq!(diff.deletions, Some(3));
        assert!(diff.diff.contains("+new line"));
    }

    #[test]
    fn test_map_file_added() {
        let gh_file = GitHubFile {
            sha: "abc".to_string(),
            filename: "new_file.rs".to_string(),
            status: "added".to_string(),
            additions: 50,
            deletions: 0,
            changes: 50,
            patch: None,
            previous_filename: None,
        };

        let diff = map_file(&gh_file);
        assert!(diff.new_file);
        assert!(!diff.deleted_file);
        assert!(diff.diff.is_empty());
    }

    #[test]
    fn test_map_file_removed() {
        let gh_file = GitHubFile {
            sha: "abc".to_string(),
            filename: "old_file.rs".to_string(),
            status: "removed".to_string(),
            additions: 0,
            deletions: 30,
            changes: 30,
            patch: None,
            previous_filename: None,
        };

        let diff = map_file(&gh_file);
        assert!(diff.deleted_file);
        assert!(!diff.new_file);
    }

    #[test]
    fn test_map_file_renamed() {
        let gh_file = GitHubFile {
            sha: "abc".to_string(),
            filename: "new_name.rs".to_string(),
            status: "renamed".to_string(),
            additions: 0,
            deletions: 0,
            changes: 0,
            patch: None,
            previous_filename: Some("old_name.rs".to_string()),
        };

        let diff = map_file(&gh_file);
        assert!(diff.renamed_file);
        assert_eq!(diff.old_path, Some("old_name.rs".to_string()));
    }

    #[test]
    fn test_map_pull_request_with_full_data() {
        let pr = GitHubPullRequest {
            id: 1,
            number: 10,
            title: "Add feature".to_string(),
            body: Some("Description".to_string()),
            state: "open".to_string(),
            html_url: "https://github.com/test/repo/pull/10".to_string(),
            draft: false,
            merged: false,
            merged_at: None,
            user: Some(GitHubUser {
                id: 1,
                login: "author".to_string(),
                name: None,
                email: None,
                avatar_url: None,
            }),
            assignees: vec![GitHubUser {
                id: 2,
                login: "assignee".to_string(),
                name: Some("Assignee".to_string()),
                email: None,
                avatar_url: None,
            }],
            requested_reviewers: vec![GitHubUser {
                id: 3,
                login: "reviewer".to_string(),
                name: None,
                email: None,
                avatar_url: None,
            }],
            labels: vec![GitHubLabel {
                id: 1,
                name: "enhancement".to_string(),
                color: None,
                description: None,
            }],
            head: GitHubBranchRef {
                ref_name: "feature-branch".to_string(),
                sha: "abc123".to_string(),
            },
            base: GitHubBranchRef {
                ref_name: "main".to_string(),
                sha: "def456".to_string(),
            },
            created_at: "2024-01-01T00:00:00Z".to_string(),
            updated_at: "2024-01-02T00:00:00Z".to_string(),
        };

        let mr = map_pull_request(&pr);
        assert_eq!(mr.key, "pr#10");
        assert_eq!(mr.title, "Add feature");
        assert_eq!(mr.description, Some("Description".to_string()));
        assert_eq!(mr.state, "open");
        assert_eq!(mr.source, "github");
        assert_eq!(mr.source_branch, "feature-branch");
        assert_eq!(mr.target_branch, "main");
        assert!(mr.author.is_some());
        assert_eq!(mr.assignees.len(), 1);
        assert_eq!(mr.assignees[0].username, "assignee");
        assert_eq!(mr.reviewers.len(), 1);
        assert_eq!(mr.reviewers[0].username, "reviewer");
        assert_eq!(mr.labels, vec!["enhancement"]);
        assert!(!mr.draft);
    }

    #[test]
    fn test_map_pull_request_merged_at() {
        let pr = GitHubPullRequest {
            id: 1,
            number: 10,
            title: "Merged PR".to_string(),
            body: None,
            state: "closed".to_string(),
            html_url: "https://github.com/test/repo/pull/10".to_string(),
            draft: false,
            merged: false,
            merged_at: Some("2024-01-03T00:00:00Z".to_string()),
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

        let mr = map_pull_request(&pr);
        assert_eq!(mr.state, "merged");
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
    fn test_map_issue_with_assignees() {
        let gh_issue = GitHubIssue {
            id: 1,
            number: 1,
            title: "Issue".to_string(),
            body: None,
            state: "open".to_string(),
            html_url: "https://github.com/test/repo/issues/1".to_string(),
            user: None,
            assignees: vec![
                GitHubUser {
                    id: 1,
                    login: "user1".to_string(),
                    name: None,
                    email: None,
                    avatar_url: None,
                },
                GitHubUser {
                    id: 2,
                    login: "user2".to_string(),
                    name: None,
                    email: None,
                    avatar_url: None,
                },
            ],
            labels: vec![],
            created_at: "2024-01-01T00:00:00Z".to_string(),
            updated_at: "2024-01-02T00:00:00Z".to_string(),
            closed_at: None,
            pull_request: None,
        };

        let issue = map_issue(&gh_issue);
        assert_eq!(issue.assignees.len(), 2);
        assert_eq!(issue.assignees[0].username, "user1");
        assert_eq!(issue.assignees[1].username, "user2");
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

    #[test]
    fn test_repo_url() {
        let client =
            GitHubClient::with_base_url("https://api.github.com", "owner", "repo", "token");
        assert_eq!(
            client.repo_url("/issues"),
            "https://api.github.com/repos/owner/repo/issues"
        );
        assert_eq!(
            client.repo_url("/pulls/1"),
            "https://api.github.com/repos/owner/repo/pulls/1"
        );
    }

    #[test]
    fn test_repo_url_strips_trailing_slash() {
        let client =
            GitHubClient::with_base_url("https://api.github.com/", "owner", "repo", "token");
        assert_eq!(
            client.repo_url("/issues"),
            "https://api.github.com/repos/owner/repo/issues"
        );
    }

    #[test]
    fn test_provider_name() {
        let client = GitHubClient::new("owner", "repo", "token");
        assert_eq!(IssueProvider::provider_name(&client), "github");
        assert_eq!(MergeRequestProvider::provider_name(&client), "github");
    }

    // =========================================================================
    // Integration tests with httpmock
    // =========================================================================

    mod integration {
        use super::*;
        use httpmock::prelude::*;

        fn create_test_client(server: &MockServer) -> GitHubClient {
            GitHubClient::with_base_url(server.base_url(), "owner", "repo", "test-token")
        }

        fn sample_issue_json() -> serde_json::Value {
            serde_json::json!({
                "id": 1,
                "number": 42,
                "title": "Test Issue",
                "body": "Issue body",
                "state": "open",
                "html_url": "https://github.com/owner/repo/issues/42",
                "user": {"id": 1, "login": "author"},
                "assignees": [],
                "labels": [{"id": 1, "name": "bug"}],
                "created_at": "2024-01-01T00:00:00Z",
                "updated_at": "2024-01-02T00:00:00Z"
            })
        }

        fn sample_pr_json() -> serde_json::Value {
            serde_json::json!({
                "id": 1,
                "number": 10,
                "title": "Test PR",
                "body": "PR body",
                "state": "open",
                "html_url": "https://github.com/owner/repo/pull/10",
                "draft": false,
                "merged": false,
                "user": {"id": 1, "login": "author"},
                "assignees": [],
                "requested_reviewers": [],
                "labels": [],
                "head": {"ref": "feature", "sha": "abc123"},
                "base": {"ref": "main", "sha": "def456"},
                "created_at": "2024-01-01T00:00:00Z",
                "updated_at": "2024-01-02T00:00:00Z"
            })
        }

        #[tokio::test]
        async fn test_get_issues() {
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(GET)
                    .path("/repos/owner/repo/issues")
                    .header("Authorization", "Bearer test-token");
                then.status(200)
                    .json_body(serde_json::json!([sample_issue_json()]));
            });

            let client = create_test_client(&server);
            let issues = client
                .get_issues(IssueFilter {
                    state: Some("open".to_string()),
                    ..Default::default()
                })
                .await
                .unwrap();

            assert_eq!(issues.len(), 1);
            assert_eq!(issues[0].key, "gh#42");
            assert_eq!(issues[0].title, "Test Issue");
        }

        #[tokio::test]
        async fn test_get_issues_filters_pull_requests() {
            let server = MockServer::start();

            let mut pr_as_issue = sample_issue_json();
            pr_as_issue["pull_request"] = serde_json::json!({"url": "..."});
            pr_as_issue["number"] = serde_json::json!(99);

            server.mock(|when, then| {
                when.method(GET).path("/repos/owner/repo/issues");
                then.status(200)
                    .json_body(serde_json::json!([sample_issue_json(), pr_as_issue]));
            });

            let client = create_test_client(&server);
            let issues = client.get_issues(IssueFilter::default()).await.unwrap();

            // Only the real issue, not the PR
            assert_eq!(issues.len(), 1);
            assert_eq!(issues[0].key, "gh#42");
        }

        #[tokio::test]
        async fn test_get_issues_with_all_filters() {
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(GET)
                    .path("/repos/owner/repo/issues")
                    .query_param("state", "closed")
                    .query_param("labels", "bug,feature")
                    .query_param("assignee", "user1")
                    .query_param("per_page", "10")
                    .query_param("page", "2")
                    .query_param("sort", "created")
                    .query_param("direction", "asc");
                then.status(200).json_body(serde_json::json!([]));
            });

            let client = create_test_client(&server);
            let issues = client
                .get_issues(IssueFilter {
                    state: Some("closed".to_string()),
                    labels: Some(vec!["bug".to_string(), "feature".to_string()]),
                    assignee: Some("user1".to_string()),
                    limit: Some(10),
                    offset: Some(10),
                    sort_by: Some("created_at".to_string()),
                    sort_order: Some("asc".to_string()),
                    ..Default::default()
                })
                .await
                .unwrap();

            assert!(issues.is_empty());
        }

        #[tokio::test]
        async fn test_get_issue() {
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(GET).path("/repos/owner/repo/issues/42");
                then.status(200).json_body(sample_issue_json());
            });

            let client = create_test_client(&server);
            let issue = client.get_issue("gh#42").await.unwrap();

            assert_eq!(issue.key, "gh#42");
            assert_eq!(issue.title, "Test Issue");
        }

        #[tokio::test]
        async fn test_get_issue_rejects_pr() {
            let server = MockServer::start();

            let mut issue_json = sample_issue_json();
            issue_json["pull_request"] = serde_json::json!({"url": "..."});

            server.mock(|when, then| {
                when.method(GET).path("/repos/owner/repo/issues/42");
                then.status(200).json_body(issue_json);
            });

            let client = create_test_client(&server);
            let result = client.get_issue("gh#42").await;
            assert!(result.is_err());
        }

        #[tokio::test]
        async fn test_create_issue() {
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(POST)
                    .path("/repos/owner/repo/issues")
                    .body_includes("\"title\":\"New Issue\"");
                then.status(201).json_body(sample_issue_json());
            });

            let client = create_test_client(&server);
            let issue = client
                .create_issue(CreateIssueInput {
                    title: "New Issue".to_string(),
                    description: Some("Body".to_string()),
                    labels: vec!["bug".to_string()],
                    assignees: vec![],
                    priority: None,
                })
                .await
                .unwrap();

            assert_eq!(issue.key, "gh#42");
        }

        #[tokio::test]
        async fn test_update_issue() {
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(PATCH)
                    .path("/repos/owner/repo/issues/42")
                    .body_includes("\"state\":\"closed\"");
                then.status(200).json_body(sample_issue_json());
            });

            let client = create_test_client(&server);
            let issue = client
                .update_issue(
                    "gh#42",
                    UpdateIssueInput {
                        state: Some("closed".to_string()),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();

            assert_eq!(issue.key, "gh#42");
        }

        #[tokio::test]
        async fn test_update_issue_state_mapping() {
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(PATCH)
                    .path("/repos/owner/repo/issues/42")
                    .body_includes("\"state\":\"open\"");
                then.status(200).json_body(sample_issue_json());
            });

            let client = create_test_client(&server);
            let result = client
                .update_issue(
                    "gh#42",
                    UpdateIssueInput {
                        state: Some("opened".to_string()),
                        ..Default::default()
                    },
                )
                .await;

            assert!(result.is_ok());
        }

        #[tokio::test]
        async fn test_get_comments() {
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(GET)
                    .path("/repos/owner/repo/issues/42/comments");
                then.status(200).json_body(serde_json::json!([{
                    "id": 1,
                    "body": "Comment text",
                    "user": {"id": 1, "login": "commenter"},
                    "created_at": "2024-01-15T10:00:00Z"
                }]));
            });

            let client = create_test_client(&server);
            let comments = client.get_comments("gh#42").await.unwrap();

            assert_eq!(comments.len(), 1);
            assert_eq!(comments[0].body, "Comment text");
        }

        #[tokio::test]
        async fn test_add_comment() {
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(POST)
                    .path("/repos/owner/repo/issues/42/comments")
                    .body_includes("\"body\":\"My comment\"");
                then.status(201).json_body(serde_json::json!({
                    "id": 1,
                    "body": "My comment",
                    "user": {"id": 1, "login": "me"},
                    "created_at": "2024-01-15T10:00:00Z"
                }));
            });

            let client = create_test_client(&server);
            let comment = IssueProvider::add_comment(&client, "gh#42", "My comment")
                .await
                .unwrap();

            assert_eq!(comment.body, "My comment");
        }

        #[tokio::test]
        async fn test_get_pull_request() {
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(GET).path("/repos/owner/repo/pulls/10");
                then.status(200).json_body(sample_pr_json());
            });

            let client = create_test_client(&server);
            let mr = client.get_merge_request("pr#10").await.unwrap();

            assert_eq!(mr.key, "pr#10");
            assert_eq!(mr.title, "Test PR");
            assert_eq!(mr.source_branch, "feature");
            assert_eq!(mr.target_branch, "main");
        }

        #[tokio::test]
        async fn test_get_pull_requests() {
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(GET).path("/repos/owner/repo/pulls");
                then.status(200)
                    .json_body(serde_json::json!([sample_pr_json()]));
            });

            let client = create_test_client(&server);
            let mrs = client
                .get_merge_requests(MrFilter::default())
                .await
                .unwrap();

            assert_eq!(mrs.len(), 1);
            assert_eq!(mrs[0].key, "pr#10");
        }

        #[tokio::test]
        async fn test_get_pull_requests_with_filters() {
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(GET)
                    .path("/repos/owner/repo/pulls")
                    .query_param("state", "closed")
                    .query_param("head", "feature")
                    .query_param("base", "main")
                    .query_param("per_page", "5");
                then.status(200).json_body(serde_json::json!([]));
            });

            let client = create_test_client(&server);
            let mrs = client
                .get_merge_requests(MrFilter {
                    state: Some("closed".to_string()),
                    source_branch: Some("feature".to_string()),
                    target_branch: Some("main".to_string()),
                    limit: Some(5),
                    ..Default::default()
                })
                .await
                .unwrap();

            assert!(mrs.is_empty());
        }

        #[tokio::test]
        async fn test_get_pull_requests_merged_filter() {
            let server = MockServer::start();

            let mut merged_pr = sample_pr_json();
            merged_pr["merged"] = serde_json::json!(true);
            merged_pr["state"] = serde_json::json!("closed");

            let open_pr = sample_pr_json();

            server.mock(|when, then| {
                when.method(GET)
                    .path("/repos/owner/repo/pulls")
                    .query_param("state", "closed");
                then.status(200)
                    .json_body(serde_json::json!([merged_pr, open_pr]));
            });

            let client = create_test_client(&server);
            let mrs = client
                .get_merge_requests(MrFilter {
                    state: Some("merged".to_string()),
                    ..Default::default()
                })
                .await
                .unwrap();

            // Only merged PRs returned
            assert_eq!(mrs.len(), 1);
            assert_eq!(mrs[0].state, "merged");
        }

        #[tokio::test]
        async fn test_get_discussions() {
            let server = MockServer::start();

            // Reviews
            server.mock(|when, then| {
                when.method(GET).path("/repos/owner/repo/pulls/10/reviews");
                then.status(200).json_body(serde_json::json!([{
                    "id": 1,
                    "user": {"id": 1, "login": "reviewer"},
                    "body": "LGTM",
                    "state": "APPROVED",
                    "submitted_at": "2024-01-15T10:00:00Z"
                }]));
            });

            // Review comments
            server.mock(|when, then| {
                when.method(GET).path("/repos/owner/repo/pulls/10/comments");
                then.status(200).json_body(serde_json::json!([{
                    "id": 100,
                    "body": "Fix this line",
                    "user": {"id": 2, "login": "reviewer2"},
                    "created_at": "2024-01-15T11:00:00Z",
                    "path": "src/main.rs",
                    "line": 42,
                    "side": "RIGHT"
                }]));
            });

            // Issue comments
            server.mock(|when, then| {
                when.method(GET)
                    .path("/repos/owner/repo/issues/10/comments");
                then.status(200).json_body(serde_json::json!([{
                    "id": 200,
                    "body": "General comment",
                    "user": {"id": 3, "login": "user3"},
                    "created_at": "2024-01-15T12:00:00Z"
                }]));
            });

            let client = create_test_client(&server);
            let discussions = client.get_discussions("pr#10").await.unwrap();

            // 1 review comment thread + 1 review + 1 general comment = 3
            assert_eq!(discussions.len(), 3);
        }

        #[tokio::test]
        async fn test_get_diffs() {
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(GET).path("/repos/owner/repo/pulls/10/files");
                then.status(200).json_body(serde_json::json!([{
                    "sha": "abc123",
                    "filename": "src/main.rs",
                    "status": "modified",
                    "additions": 10,
                    "deletions": 3,
                    "changes": 13,
                    "patch": "@@ +new code"
                }]));
            });

            let client = create_test_client(&server);
            let diffs = client.get_diffs("pr#10").await.unwrap();

            assert_eq!(diffs.len(), 1);
            assert_eq!(diffs[0].file_path, "src/main.rs");
            assert_eq!(diffs[0].additions, Some(10));
        }

        #[tokio::test]
        async fn test_add_mr_comment_general() {
            let server = MockServer::start();

            // PR lookup
            server.mock(|when, then| {
                when.method(GET).path("/repos/owner/repo/pulls/10");
                then.status(200).json_body(sample_pr_json());
            });

            // Create comment
            server.mock(|when, then| {
                when.method(POST)
                    .path("/repos/owner/repo/issues/10/comments");
                then.status(201).json_body(serde_json::json!({
                    "id": 1,
                    "body": "General comment",
                    "user": {"id": 1, "login": "me"},
                    "created_at": "2024-01-15T10:00:00Z"
                }));
            });

            let client = create_test_client(&server);
            let comment = MergeRequestProvider::add_comment(
                &client,
                "pr#10",
                CreateCommentInput {
                    body: "General comment".to_string(),
                    position: None,
                    discussion_id: None,
                },
            )
            .await
            .unwrap();

            assert_eq!(comment.body, "General comment");
        }

        #[tokio::test]
        async fn test_add_mr_comment_inline() {
            let server = MockServer::start();

            // PR lookup
            server.mock(|when, then| {
                when.method(GET).path("/repos/owner/repo/pulls/10");
                then.status(200).json_body(sample_pr_json());
            });

            // Create review comment
            server.mock(|when, then| {
                when.method(POST)
                    .path("/repos/owner/repo/pulls/10/comments")
                    .body_includes("\"path\":\"src/main.rs\"")
                    .body_includes("\"line\":42");
                then.status(201).json_body(serde_json::json!({
                    "id": 1,
                    "body": "Inline comment",
                    "user": {"id": 1, "login": "me"},
                    "created_at": "2024-01-15T10:00:00Z",
                    "path": "src/main.rs",
                    "line": 42,
                    "side": "RIGHT"
                }));
            });

            let client = create_test_client(&server);
            let comment = MergeRequestProvider::add_comment(
                &client,
                "pr#10",
                CreateCommentInput {
                    body: "Inline comment".to_string(),
                    position: Some(CodePosition {
                        file_path: "src/main.rs".to_string(),
                        line: 42,
                        line_type: "new".to_string(),
                        commit_sha: Some("abc123".to_string()),
                    }),
                    discussion_id: None,
                },
            )
            .await
            .unwrap();

            assert_eq!(comment.body, "Inline comment");
        }

        #[tokio::test]
        async fn test_handle_response_401() {
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(GET).path("/repos/owner/repo/issues");
                then.status(401).body("Bad credentials");
            });

            let client = create_test_client(&server);
            let result = client.get_issues(IssueFilter::default()).await;

            assert!(result.is_err());
            let err = result.unwrap_err();
            assert!(matches!(err, Error::Unauthorized(_)));
        }

        #[tokio::test]
        async fn test_handle_response_404() {
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(GET).path("/repos/owner/repo/issues/999");
                then.status(404).body("Not Found");
            });

            let client = create_test_client(&server);
            let result = client.get_issue("gh#999").await;

            assert!(result.is_err());
            let err = result.unwrap_err();
            assert!(matches!(err, Error::NotFound(_)));
        }

        #[tokio::test]
        async fn test_handle_response_500() {
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(GET).path("/repos/owner/repo/issues");
                then.status(500).body("Internal Server Error");
            });

            let client = create_test_client(&server);
            let result = client.get_issues(IssueFilter::default()).await;

            assert!(result.is_err());
            let err = result.unwrap_err();
            assert!(matches!(err, Error::ServerError { .. }));
        }

        #[tokio::test]
        async fn test_get_current_user() {
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(GET).path("/user");
                then.status(200).json_body(serde_json::json!({
                    "id": 1,
                    "login": "testuser",
                    "name": "Test User",
                    "email": "test@example.com"
                }));
            });

            let client = create_test_client(&server);
            let user = client.get_current_user().await.unwrap();

            assert_eq!(user.username, "testuser");
            assert_eq!(user.name, Some("Test User".to_string()));
        }
    }
}
