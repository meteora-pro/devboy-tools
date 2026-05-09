//! GitHub API client implementation.

use async_trait::async_trait;
use devboy_core::{
    AssetCapabilities, AssetMeta, CodePosition, Comment, ContextCapabilities, CreateCommentInput,
    CreateIssueInput, CreateMergeRequestInput, Discussion, Error, FailedJob, FileDiff,
    GetPipelineInput, Issue, IssueFilter, IssueProvider, JobLogMode, JobLogOptions, JobLogOutput,
    MergeRequest, MergeRequestProvider, MrFilter, PipelineInfo, PipelineJob, PipelineProvider,
    PipelineStage, PipelineStatus, PipelineSummary, Provider, ProviderResult, Result,
    UpdateIssueInput, UpdateMergeRequestInput, User, parse_markdown_attachments,
};
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use tracing::{debug, warn};

use crate::DEFAULT_GITHUB_URL;
use crate::types::{
    CreateCommentRequest, CreateIssueRequest, CreatePullRequestRequest, CreateReviewCommentRequest,
    GitHubComment, GitHubFile, GitHubIssue, GitHubLabel, GitHubPullRequest, GitHubReview,
    GitHubReviewComment, GitHubUser, UpdateIssueRequest, UpdatePullRequestRequest,
};

/// GitHub API client.
pub struct GitHubClient {
    base_url: String,
    owner: String,
    repo: String,
    token: SecretString,
    client: reqwest::Client,
}

impl GitHubClient {
    /// Create a new GitHub client.
    pub fn new(owner: impl Into<String>, repo: impl Into<String>, token: SecretString) -> Self {
        Self::with_base_url(DEFAULT_GITHUB_URL, owner, repo, token)
    }

    /// Create a new GitHub client with a custom base URL.
    pub fn with_base_url(
        base_url: impl Into<String>,
        owner: impl Into<String>,
        repo: impl Into<String>,
        token: SecretString,
    ) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            owner: owner.into(),
            repo: repo.into(),
            token,
            client: reqwest::Client::builder()
                .user_agent("devboy-tools")
                .build()
                .expect("Failed to create HTTP client"),
        }
    }

    /// Base URL the client was configured against. Public so the
    /// liveness probe (and any future sibling module) can build
    /// its own requests without re-walking the constructor.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Borrow the underlying [`reqwest::Client`]. Same rationale as
    /// [`Self::base_url`] — the liveness probe issues its own
    /// auth-introspection request and reuses the connection pool.
    pub fn http_client(&self) -> &reqwest::Client {
        &self.client
    }

    /// Build request with common headers.
    fn request(&self, method: reqwest::Method, url: &str) -> reqwest::RequestBuilder {
        let mut builder = self
            .client
            .request(method, url)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28");

        let token = self.token.expose_secret();
        if !token.is_empty() {
            builder = builder.header("Authorization", format!("Bearer {}", token));
        }

        builder
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
    // Count GitHub attachment references in the body (no extra API call).
    // Uses the same detection logic as `is_github_attachment_url` so
    // both CDN hosts and `github.com/user-attachments/` URLs are counted.
    let attachments_count = gh_issue
        .body
        .as_deref()
        .map(|body| {
            parse_markdown_attachments(body)
                .iter()
                .filter(|a| is_github_attachment_url("https://github.com", &a.url))
                .count() as u32
        })
        .filter(|&c| c > 0);

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
        attachments_count,
        parent: None,
        subtasks: vec![],
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
    async fn get_issues(&self, filter: IssueFilter) -> Result<ProviderResult<Issue>> {
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

        if let Some(labels) = &filter.labels
            && !labels.is_empty()
        {
            params.push(format!("labels={}", labels.join(",")));
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

        Ok(issues.into())
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

    async fn get_comments(&self, issue_key: &str) -> Result<ProviderResult<Comment>> {
        let number = parse_issue_key(issue_key)?;
        let url = self.repo_url(&format!("/issues/{}/comments", number));
        let gh_comments: Vec<GitHubComment> = self.get(&url).await?;
        Ok(gh_comments
            .iter()
            .map(map_comment)
            .collect::<Vec<_>>()
            .into())
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

    async fn get_issue_attachments(&self, issue_key: &str) -> Result<Vec<AssetMeta>> {
        // GitHub does not expose an attachment API for issues; we parse the
        // issue body and all comment bodies for markdown-embedded files.
        let issue = self.get_issue(issue_key).await?;
        let comments = self.get_comments(issue_key).await?;

        let mut attachments: Vec<AssetMeta> = Vec::new();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let base = self.base_url.clone();
        let mut collect = |source: &str| {
            for att in parse_markdown_attachments(source) {
                // Only include URLs that point to known GitHub CDN /
                // upload hosts. Ordinary markdown links (docs, issues,
                // dashboards) must not appear as downloadable attachments.
                if is_github_attachment_url(&base, &att.url) && seen.insert(att.url.clone()) {
                    attachments.push(markdown_to_meta(&att));
                }
            }
        };
        if let Some(body) = issue.description.as_deref() {
            collect(body);
        }
        for comment in &comments.items {
            collect(&comment.body);
        }
        Ok(attachments)
    }

    async fn download_attachment(&self, _issue_key: &str, asset_id: &str) -> Result<Vec<u8>> {
        download_github_url(&self.client, &self.base_url, &self.token, asset_id).await
    }

    fn asset_capabilities(&self) -> AssetCapabilities {
        // GitHub has no public file upload API for issues / PRs (files are
        // only uploaded through the web UI to a CDN). We support download
        // and list via markdown parsing; upload / delete stay false.
        let caps = ContextCapabilities {
            upload: false,
            download: true,
            delete: false,
            list: true,
            max_file_size: None,
            allowed_types: Vec::new(),
        };
        AssetCapabilities {
            issue: caps.clone(),
            issue_comment: caps.clone(),
            merge_request: caps.clone(),
            mr_comment: caps,
        }
    }

    fn provider_name(&self) -> &'static str {
        "github"
    }
}

#[async_trait]
impl MergeRequestProvider for GitHubClient {
    async fn get_merge_requests(&self, filter: MrFilter) -> Result<ProviderResult<MergeRequest>> {
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

        Ok(prs.into())
    }

    async fn get_merge_request(&self, key: &str) -> Result<MergeRequest> {
        let number = parse_pr_key(key)?;
        let url = self.repo_url(&format!("/pulls/{}", number));
        let gh_pr: GitHubPullRequest = self.get(&url).await?;
        Ok(map_pull_request(&gh_pr))
    }

    async fn get_discussions(&self, mr_key: &str) -> Result<ProviderResult<Discussion>> {
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
            if let Some(body) = &review.body
                && !body.is_empty()
            {
                comments.push(Comment {
                    id: review.id.to_string(),
                    body: body.clone(),
                    author: map_user(review.user.as_ref()),
                    created_at: review.submitted_at.clone(),
                    updated_at: None,
                    position: None,
                });
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

        Ok(discussions.into())
    }

    async fn get_diffs(&self, mr_key: &str) -> Result<ProviderResult<FileDiff>> {
        let number = parse_pr_key(mr_key)?;
        let url = self.repo_url(&format!("/pulls/{}/files", number));
        let gh_files: Vec<GitHubFile> = self.get(&url).await?;
        Ok(gh_files.iter().map(map_file).collect::<Vec<_>>().into())
    }

    async fn add_comment(&self, mr_key: &str, input: CreateCommentInput) -> Result<Comment> {
        let number = parse_pr_key(mr_key)?;

        // First verify that this is actually a PR, not an issue
        let pr_url = self.repo_url(&format!("/pulls/{}", number));
        let pr_result: Result<GitHubPullRequest> = self.get(&pr_url).await;

        if let Err(Error::Http(status)) = &pr_result
            && status.contains("404")
        {
            return Err(Error::InvalidData(format!(
                "{} is not a valid pull request (it may be an issue)",
                mr_key
            )));
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
                // Unified `Discussion.id` is prefixed (`review-<n>` for a
                // review thread, `comment-<n>` for a general issue
                // comment) — see `get_discussions` below. Strip either
                // prefix before parsing so callers can feed the id they
                // received from `get_merge_request_discussions` straight
                // back into `create_merge_request_comment` and have the
                // new comment actually thread into the existing review.
                in_reply_to: input
                    .discussion_id
                    .as_deref()
                    .and_then(parse_discussion_numeric_id),
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

    async fn create_merge_request(&self, input: CreateMergeRequestInput) -> Result<MergeRequest> {
        let url = self.repo_url("/pulls");

        let request = CreatePullRequestRequest {
            title: input.title,
            body: input.description,
            head: input.source_branch,
            base: input.target_branch,
            draft: if input.draft { Some(true) } else { None },
        };

        let gh_pr: GitHubPullRequest = self.post(&url, &request).await?;

        // Add labels if provided (best-effort: PR is already created)
        if !input.labels.is_empty() {
            let labels_url = self.repo_url(&format!("/issues/{}/labels", gh_pr.number));
            let result: Result<serde_json::Value> = self
                .post(&labels_url, &serde_json::json!({ "labels": input.labels }))
                .await;
            if let Err(err) = result {
                warn!(
                    error = ?err,
                    pr_number = gh_pr.number,
                    "Failed to add labels to GitHub pull request"
                );
            }
        }

        // Add reviewers if provided (best-effort: PR is already created)
        if !input.reviewers.is_empty() {
            let reviewers_url =
                self.repo_url(&format!("/pulls/{}/requested_reviewers", gh_pr.number));
            let result: Result<serde_json::Value> = self
                .post(
                    &reviewers_url,
                    &serde_json::json!({ "reviewers": input.reviewers }),
                )
                .await;
            if let Err(err) = result {
                warn!(
                    error = ?err,
                    pr_number = gh_pr.number,
                    "Failed to add reviewers to GitHub pull request"
                );
            }
        }

        // Re-fetch the PR to get updated labels/reviewers (best-effort)
        if !input.labels.is_empty() || !input.reviewers.is_empty() {
            let pr_url = self.repo_url(&format!("/pulls/{}", gh_pr.number));
            match self.get::<GitHubPullRequest>(&pr_url).await {
                Ok(updated_pr) => return Ok(map_pull_request(&updated_pr)),
                Err(err) => {
                    warn!(
                        error = ?err,
                        pr_number = gh_pr.number,
                        "Failed to re-fetch GitHub pull request"
                    );
                }
            }
        }

        Ok(map_pull_request(&gh_pr))
    }

    async fn update_merge_request(
        &self,
        key: &str,
        input: UpdateMergeRequestInput,
    ) -> Result<MergeRequest> {
        let number = parse_pr_key(key)?;
        let url = self.repo_url(&format!("/pulls/{}", number));

        // Map state: GitHub uses "open" / "closed".
        let state = input.state.map(|s| match s.as_str() {
            "opened" | "open" | "reopen" => "open".to_string(),
            "closed" | "close" => "closed".to_string(),
            _ => s,
        });

        let request = UpdatePullRequestRequest {
            title: input.title,
            body: input.description,
            state,
            draft: input.draft,
        };

        let gh_pr: GitHubPullRequest = self.patch(&url, &request).await?;

        // Update labels if provided (best-effort: PR is already updated).
        if let Some(labels) = input.labels {
            let labels_url = self.repo_url(&format!("/issues/{}/labels", number));
            let result: Result<serde_json::Value> = self
                .patch(&labels_url, &serde_json::json!({ "labels": labels }))
                .await;
            if let Err(err) = result {
                warn!(
                    error = ?err,
                    pr_number = number,
                    "Failed to update labels on GitHub pull request"
                );
            }

            // Re-fetch to include updated labels.
            let pr_url = self.repo_url(&format!("/pulls/{}", number));
            match self.get::<GitHubPullRequest>(&pr_url).await {
                Ok(updated_pr) => return Ok(map_pull_request(&updated_pr)),
                Err(err) => {
                    warn!(
                        error = ?err,
                        pr_number = number,
                        "Failed to re-fetch GitHub pull request"
                    );
                }
            }
        }

        Ok(map_pull_request(&gh_pr))
    }

    async fn get_mr_attachments(&self, mr_key: &str) -> Result<Vec<AssetMeta>> {
        let mr = self.get_merge_request(mr_key).await?;
        let discussions = self.get_discussions(mr_key).await?;

        let mut attachments: Vec<AssetMeta> = Vec::new();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let base = self.base_url.clone();
        let mut collect = |source: &str| {
            for att in parse_markdown_attachments(source) {
                if is_github_attachment_url(&base, &att.url) && seen.insert(att.url.clone()) {
                    attachments.push(markdown_to_meta(&att));
                }
            }
        };
        if let Some(body) = mr.description.as_deref() {
            collect(body);
        }
        for discussion in &discussions.items {
            for comment in &discussion.comments {
                collect(&comment.body);
            }
        }
        Ok(attachments)
    }

    async fn download_mr_attachment(&self, _mr_key: &str, asset_id: &str) -> Result<Vec<u8>> {
        download_github_url(&self.client, &self.base_url, &self.token, asset_id).await
    }

    fn provider_name(&self) -> &'static str {
        "github"
    }
}

/// Convert a parsed markdown attachment into an [`AssetMeta`] record.
///
/// GitHub has no stable attachment id — the URL itself doubles as both the
/// lookup key and the download target.
/// Known GitHub-owned hosts that are safe to send auth headers to.
const GITHUB_TRUSTED_HOSTS: &[&str] = &[
    "github.com",
    "api.github.com",
    "githubusercontent.com",
    "user-images.githubusercontent.com",
    "raw.githubusercontent.com",
    "objects.githubusercontent.com",
    "camo.githubusercontent.com",
];

/// Download a URL, attaching GitHub auth headers only when the host
/// requires it. CDN hosts (`*.githubusercontent.com`) serve content
/// anonymously — sending a Bearer token to their S3 backend causes
/// `400 Unsupported Authorization Type`.
async fn download_github_url(
    client: &reqwest::Client,
    base_url: &str,
    token: &SecretString,
    url: &str,
) -> Result<Vec<u8>> {
    let needs_auth = is_github_api_host(base_url, url);
    let mut request = client
        .get(url)
        .header("Accept", "application/octet-stream")
        .header("User-Agent", "devboy-tools");
    let token_value = token.expose_secret();
    if needs_auth && !token_value.is_empty() {
        request = request.header("Authorization", format!("Bearer {token_value}"));
    } else if !is_github_trusted_host(base_url, url) {
        tracing::warn!(
            url,
            "downloading cross-origin attachment without auth headers"
        );
    }
    let response = request
        .send()
        .await
        .map_err(|e| Error::Http(e.to_string()))?;
    let status = response.status();
    if !status.is_success() {
        let message = response.text().await.unwrap_or_default();
        return Err(Error::from_status(status.as_u16(), message));
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|e| Error::Http(format!("failed to read attachment bytes: {e}")))?;
    Ok(bytes.to_vec())
}

/// Check whether a URL points to a GitHub API host that needs
/// Authorization headers. CDN hosts (*.githubusercontent.com) do NOT
/// need auth — they serve content anonymously via S3-style presigned
/// URLs and reject Bearer tokens.
fn is_github_api_host(base_url: &str, url: &str) -> bool {
    let (url_scheme, url_host) = split_scheme_host(url);
    if url_scheme != "https" {
        return false;
    }
    // API hosts that accept Bearer tokens.
    if url_host == "api.github.com" || url_host == "github.com" {
        return true;
    }
    // GitHub Enterprise: base_url host.
    let (_base_scheme, base_host) = split_scheme_host(base_url);
    url_host == base_host
}

/// Check whether a URL is a known GitHub host or matches the configured
/// base URL (for GitHub Enterprise instances).
///
/// Only HTTPS URLs are trusted — a `http://github.com/...` link would
/// send credentials over plaintext and is rejected.
fn is_github_trusted_host(base_url: &str, url: &str) -> bool {
    let (url_scheme, url_host) = split_scheme_host(url);
    if url_scheme != "https" {
        return false;
    }

    // Check against well-known GitHub CDN hosts.
    for trusted in GITHUB_TRUSTED_HOSTS {
        if url_host == *trusted || url_host.ends_with(&format!(".{trusted}")) {
            return true;
        }
    }

    // Check against the configured base URL (GitHub Enterprise).
    let (_base_scheme, base_host) = split_scheme_host(base_url);
    url_host == base_host
}

/// Extract (scheme, host) from a URL string, both lowercased.
fn split_scheme_host(url: &str) -> (String, String) {
    let (scheme, rest) = match url.split_once("://") {
        Some((s, r)) => (s.to_ascii_lowercase(), r),
        None => return (String::new(), String::new()),
    };
    let host = rest.split('/').next().unwrap_or("").to_ascii_lowercase();
    (scheme, host)
}

/// Check whether a URL looks like a real GitHub file attachment (CDN
/// upload, user-content image, etc.) as opposed to an ordinary markdown
/// link to a docs page, issue, or dashboard.
///
/// GitHub user-uploaded attachments are hosted on `githubusercontent.com`
/// subdomains. We also accept `/assets/` paths on the configured host
/// (GitHub Enterprise may serve uploads from the same domain).
fn is_github_attachment_url(base_url: &str, url: &str) -> bool {
    let (scheme, host) = split_scheme_host(url);
    if scheme.is_empty() {
        return false; // relative path — not a CDN upload
    }
    // Well-known GitHub CDN hosts for user-uploaded content.
    if host.ends_with("githubusercontent.com") {
        return true;
    }
    // github.com/user-attachments/assets/ — new upload format (Web UI).
    if host == "github.com" {
        let path = url
            .split("://")
            .nth(1)
            .unwrap_or("")
            .split_once('/')
            .map(|(_, p)| p)
            .unwrap_or("");
        if path.starts_with("user-attachments/assets/")
            || path.starts_with("user-attachments/files/")
        {
            return true;
        }
    }
    // On the base host: only `/assets/` paths are real uploads.
    let (_base_scheme, base_host) = split_scheme_host(base_url);
    if host == base_host {
        let path = url
            .split("://")
            .nth(1)
            .unwrap_or("")
            .split_once('/')
            .map(|(_, p)| p)
            .unwrap_or("");
        return path.contains("/assets/");
    }
    false
}

fn markdown_to_meta(att: &devboy_core::MarkdownAttachment) -> AssetMeta {
    AssetMeta {
        id: att.url.clone(),
        filename: att.filename.clone(),
        mime_type: None,
        size: None,
        url: Some(att.url.clone()),
        created_at: None,
        author: None,
        cached: false,
        local_path: None,
        checksum_sha256: None,
        analysis: None,
    }
}

// =============================================================================
// Pipeline Provider (GitHub Actions)
// =============================================================================

/// GitHub Actions workflow run.
#[derive(Debug, Deserialize)]
struct GhWorkflowRun {
    id: u64,
    name: Option<String>,
    status: Option<String>,
    conclusion: Option<String>,
    #[allow(dead_code)]
    head_branch: Option<String>,
    head_sha: String,
    html_url: String,
    run_started_at: Option<String>,
    updated_at: Option<String>,
}

/// GitHub Actions workflow runs list.
#[derive(Debug, Deserialize)]
struct GhWorkflowRuns {
    workflow_runs: Vec<GhWorkflowRun>,
}

/// GitHub Actions job.
#[derive(Debug, Deserialize)]
struct GhJob {
    id: u64,
    name: String,
    status: Option<String>,
    conclusion: Option<String>,
    html_url: Option<String>,
    started_at: Option<String>,
    completed_at: Option<String>,
}

/// GitHub Actions jobs list.
#[derive(Debug, Deserialize)]
struct GhJobs {
    jobs: Vec<GhJob>,
}

fn map_gh_status(status: Option<&str>, conclusion: Option<&str>) -> PipelineStatus {
    match (status, conclusion) {
        (Some("completed"), Some("success")) => PipelineStatus::Success,
        (Some("completed"), Some("failure")) => PipelineStatus::Failed,
        (Some("completed"), Some("cancelled")) => PipelineStatus::Canceled,
        (Some("completed"), Some("skipped")) => PipelineStatus::Skipped,
        (Some("in_progress"), _) => PipelineStatus::Running,
        (Some("queued"), _) | (Some("waiting"), _) => PipelineStatus::Pending,
        _ => PipelineStatus::Unknown,
    }
}

fn estimate_duration(started: Option<&str>, completed: Option<&str>) -> Option<u64> {
    let start = started?.parse::<chrono::DateTime<chrono::Utc>>().ok()?;
    let end = completed?.parse::<chrono::DateTime<chrono::Utc>>().ok()?;
    Some(
        end.signed_duration_since(start)
            .num_seconds()
            .unsigned_abs(),
    )
}

/// Strip ANSI escape codes from log text.
fn strip_ansi(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            // Skip until 'm' (SGR) or letter
            while let Some(&next) = chars.peek() {
                chars.next();
                if next.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            result.push(ch);
        }
    }
    result
}

/// Extract error lines from job log using common patterns.
fn extract_errors(log: &str, max_lines: usize) -> Option<String> {
    let patterns = [
        "error[",
        "error:",
        "FAILED",
        "Error:",
        "panic",
        "FATAL",
        "AssertionError",
        "TypeError",
        "Cannot find",
        "not found",
        "exit code",
    ];
    let lines: Vec<&str> = log.lines().collect();
    let mut error_lines: Vec<String> = Vec::new();

    for (i, line) in lines.iter().enumerate() {
        let stripped = strip_ansi(line);
        if patterns.iter().any(|p| stripped.contains(p)) {
            // Add context: 2 lines before + match + 2 lines after
            let start = i.saturating_sub(2);
            let end = (i + 3).min(lines.len());
            for ctx_line_raw in &lines[start..end] {
                let ctx_line = strip_ansi(ctx_line_raw).trim().to_string();
                if !ctx_line.is_empty() && !error_lines.contains(&ctx_line) {
                    error_lines.push(ctx_line);
                }
            }
            if error_lines.len() >= max_lines {
                break;
            }
        }
    }

    if error_lines.is_empty() {
        // Fallback: last 10 non-empty lines
        let tail: Vec<String> = lines
            .iter()
            .rev()
            .filter_map(|l| {
                let s = strip_ansi(l).trim().to_string();
                if s.is_empty() { None } else { Some(s) }
            })
            .take(10)
            .collect();
        if tail.is_empty() {
            None
        } else {
            Some(tail.into_iter().rev().collect::<Vec<_>>().join("\n"))
        }
    } else {
        Some(error_lines.join("\n"))
    }
}

#[async_trait]
impl PipelineProvider for GitHubClient {
    fn provider_name(&self) -> &'static str {
        "github"
    }

    async fn get_pipeline(&self, input: GetPipelineInput) -> Result<PipelineInfo> {
        // Resolve which branch to query
        let branch = if let Some(ref mr_key) = input.mr_key {
            // pr#123 → get PR head branch
            let number = parse_pr_key(mr_key)?;
            let pr_url = self.repo_url(&format!("/pulls/{number}"));
            let pr: GitHubPullRequest = self.get(&pr_url).await?;
            pr.head.ref_name
        } else if let Some(ref branch) = input.branch {
            branch.clone()
        } else {
            // Default: main branch
            "main".to_string()
        };

        // Get latest workflow run for this branch
        let runs_url = self.repo_url(&format!(
            "/actions/runs?branch={}&per_page=1&status=completed",
            urlencoding::encode(&branch)
        ));
        let runs: GhWorkflowRuns = self.get(&runs_url).await?;

        // Also check in-progress runs
        let active_runs_url = self.repo_url(&format!(
            "/actions/runs?branch={}&per_page=1&status=in_progress",
            urlencoding::encode(&branch)
        ));
        let active_runs: GhWorkflowRuns =
            self.get(&active_runs_url).await.unwrap_or(GhWorkflowRuns {
                workflow_runs: vec![],
            });

        // Pick the most recent run (prefer in-progress over completed)
        let run = active_runs
            .workflow_runs
            .into_iter()
            .chain(runs.workflow_runs)
            .next()
            .ok_or_else(|| {
                Error::NotFound(format!("No workflow runs found for branch '{branch}'"))
            })?;

        let run_status = map_gh_status(run.status.as_deref(), run.conclusion.as_deref());

        // Get jobs for this run
        let jobs_url = self.repo_url(&format!("/actions/runs/{}/jobs?per_page=100", run.id));
        let gh_jobs: GhJobs = self.get(&jobs_url).await?;

        // Build summary
        let mut summary = PipelineSummary {
            total: gh_jobs.jobs.len() as u32,
            ..Default::default()
        };

        // Group jobs by workflow name (use run name as single stage)
        let mut jobs: Vec<PipelineJob> = Vec::new();
        let mut failed_job_ids: Vec<(u64, String)> = Vec::new();

        for job in &gh_jobs.jobs {
            let status = map_gh_status(job.status.as_deref(), job.conclusion.as_deref());
            match status {
                PipelineStatus::Success => summary.success += 1,
                PipelineStatus::Failed => {
                    summary.failed += 1;
                    failed_job_ids.push((job.id, job.name.clone()));
                }
                PipelineStatus::Running => summary.running += 1,
                PipelineStatus::Pending => summary.pending += 1,
                PipelineStatus::Canceled => summary.canceled += 1,
                PipelineStatus::Skipped => summary.skipped += 1,
                PipelineStatus::Unknown => {}
            }

            let duration =
                estimate_duration(job.started_at.as_deref(), job.completed_at.as_deref());

            jobs.push(PipelineJob {
                id: job.id.to_string(),
                name: job.name.clone(),
                status,
                url: job.html_url.clone(),
                duration,
            });
        }

        // Fetch error snippets for failed jobs (max 5)
        let mut failed_jobs: Vec<FailedJob> = Vec::new();
        if input.include_failed_logs {
            for (job_id, job_name) in failed_job_ids.iter().take(5) {
                let log_url = self.repo_url(&format!("/actions/jobs/{job_id}/logs"));
                let error_snippet = match self.request(reqwest::Method::GET, &log_url).send().await
                {
                    Ok(resp) if resp.status().is_success() => {
                        let log_text = resp.text().await.unwrap_or_default();
                        extract_errors(&log_text, 20)
                    }
                    _ => None,
                };
                failed_jobs.push(FailedJob {
                    id: job_id.to_string(),
                    name: job_name.clone(),
                    url: None,
                    error_snippet,
                });
            }
        }

        let duration = estimate_duration(run.run_started_at.as_deref(), run.updated_at.as_deref());

        let stage_name = run.name.unwrap_or_else(|| "CI".to_string());

        Ok(PipelineInfo {
            id: run.id.to_string(),
            status: run_status,
            reference: branch,
            sha: run.head_sha,
            url: Some(run.html_url),
            duration,
            coverage: None,
            summary,
            stages: vec![PipelineStage {
                name: stage_name,
                jobs,
            }],
            failed_jobs,
        })
    }

    async fn get_job_logs(&self, job_id: &str, options: JobLogOptions) -> Result<JobLogOutput> {
        let log_url = self.repo_url(&format!("/actions/jobs/{job_id}/logs"));
        let resp = self
            .request(reqwest::Method::GET, &log_url)
            .send()
            .await
            .map_err(|e| Error::Network(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(Error::from_status(
                resp.status().as_u16(),
                format!("Failed to fetch job logs for job {job_id}"),
            ));
        }

        // GitHub may return plain text or redirect to ZIP.
        // Check Content-Type to detect binary/ZIP responses.
        let content_type = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();

        let raw_log = if content_type.contains("application/zip")
            || content_type.contains("application/octet-stream")
        {
            // Binary/ZIP response — return error message instead of garbled output
            return Err(Error::InvalidData(
                "Job logs returned as ZIP archive. This typically happens for large logs. \
                 Try using pattern search mode to find specific errors."
                    .to_string(),
            ));
        } else {
            resp.text()
                .await
                .map_err(|e| Error::Network(e.to_string()))?
        };
        let log = strip_ansi(&raw_log);
        let lines: Vec<&str> = log.lines().collect();
        let total_lines = lines.len();

        let (content, mode_name) = match options.mode {
            JobLogMode::Smart => {
                let extracted = extract_errors(&log, 30).unwrap_or_else(|| {
                    lines
                        .iter()
                        .rev()
                        .take(20)
                        .copied()
                        .collect::<Vec<_>>()
                        .into_iter()
                        .rev()
                        .collect::<Vec<_>>()
                        .join("\n")
                });
                (extracted, "smart")
            }
            JobLogMode::Search {
                ref pattern,
                context,
                max_matches,
            } => {
                let re = regex::Regex::new(pattern)
                    .unwrap_or_else(|_| regex::Regex::new(&regex::escape(pattern)).unwrap());
                let mut matches = Vec::new();
                for (i, line) in lines.iter().enumerate() {
                    if re.is_match(line) {
                        let start = i.saturating_sub(context);
                        let end = (i + context + 1).min(total_lines);
                        matches.push(format!("--- Match at line {} ---", i + 1));
                        for (j, ctx_line) in lines[start..end].iter().enumerate() {
                            let line_num = start + j;
                            let marker = if line_num == i { ">>>" } else { "   " };
                            matches.push(format!("{} {}: {}", marker, line_num + 1, ctx_line));
                        }
                        if matches.len() / (context * 2 + 2) >= max_matches {
                            break;
                        }
                    }
                }
                (matches.join("\n"), "search")
            }
            JobLogMode::Paginated { offset, limit } => {
                let page: Vec<&str> = lines.iter().skip(offset).take(limit).copied().collect();
                (page.join("\n"), "paginated")
            }
            JobLogMode::Full { max_lines } => {
                let truncated: Vec<&str> = lines.iter().take(max_lines).copied().collect();
                (truncated.join("\n"), "full")
            }
        };

        Ok(JobLogOutput {
            job_id: job_id.to_string(),
            job_name: None,
            content,
            mode: mode_name.to_string(),
            total_lines: Some(total_lines),
        })
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

/// Turn a unified `Discussion.id` back into the numeric comment id
/// GitHub expects in `in_reply_to`. `get_discussions` emits three
/// prefix shapes:
///
/// - `thread-<n>` for multi-comment review threads (one per root
///   review comment, grouped by `in_reply_to_id`) — this is the id
///   most skills actually feed back into `create_merge_request_comment`
///   when they want their reply to thread.
/// - `review-<n>` for single-comment review bodies.
/// - `comment-<n>` for general PR comments (note: GitHub itself does
///   not thread those, but stripping the prefix keeps the parser
///   lossless and lets the caller pass the numeric id elsewhere).
///
/// Raw numeric strings pass through unchanged for forward
/// compatibility and for test fixtures constructed by hand.
fn parse_discussion_numeric_id(id: &str) -> Option<u64> {
    let trimmed = id
        .strip_prefix("thread-")
        .or_else(|| id.strip_prefix("review-"))
        .or_else(|| id.strip_prefix("comment-"))
        .unwrap_or(id);
    trimmed.parse::<u64>().ok()
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
    fn test_parse_discussion_numeric_id_strips_prefixes() {
        // Regression for #188 bug #6/#18: Discussion.id returned by
        // get_discussions is prefixed. Callers feed it straight back
        // into create_merge_request_comment expecting it to thread.
        //
        // `get_discussions` actually emits three prefix shapes — the
        // `thread-` form covers multi-comment review threads and is
        // the one most skills pass back when they reply. All three
        // must decode to the numeric comment id.
        assert_eq!(
            parse_discussion_numeric_id("thread-3694869522"),
            Some(3694869522)
        );
        assert_eq!(
            parse_discussion_numeric_id("review-3694869522"),
            Some(3694869522)
        );
        assert_eq!(
            parse_discussion_numeric_id("comment-4147511088"),
            Some(4147511088)
        );
        // Raw numeric id passes through (forward compat).
        assert_eq!(parse_discussion_numeric_id("12345"), Some(12345));
        // Unknown prefix / non-numeric tail yields None — in_reply_to
        // stays unset and we fall back to a standalone comment rather
        // than panicking.
        assert_eq!(parse_discussion_numeric_id("weird-42"), None);
        assert_eq!(parse_discussion_numeric_id("review-notnumeric"), None);
        assert_eq!(parse_discussion_numeric_id(""), None);
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

    fn token(s: &str) -> SecretString {
        SecretString::from(s.to_string())
    }

    #[test]
    fn test_repo_url() {
        let client =
            GitHubClient::with_base_url("https://api.github.com", "owner", "repo", token("token"));
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
            GitHubClient::with_base_url("https://api.github.com/", "owner", "repo", token("token"));
        assert_eq!(
            client.repo_url("/issues"),
            "https://api.github.com/repos/owner/repo/issues"
        );
    }

    #[test]
    fn test_provider_name() {
        let client = GitHubClient::new("owner", "repo", token("token"));
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
            GitHubClient::with_base_url(server.base_url(), "owner", "repo", token("test-token"))
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
                .unwrap()
                .items;

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
            let issues = client
                .get_issues(IssueFilter::default())
                .await
                .unwrap()
                .items;

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
                .unwrap()
                .items;

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
                    ..Default::default()
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
            let comments = client.get_comments("gh#42").await.unwrap().items;

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
                .unwrap()
                .items;

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
                .unwrap()
                .items;

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
                .unwrap()
                .items;

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
            let discussions = client.get_discussions("pr#10").await.unwrap().items;

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
            let diffs = client.get_diffs("pr#10").await.unwrap().items;

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

        // =====================================================================
        // Pipeline tests
        // =====================================================================

        fn sample_workflow_run_json() -> serde_json::Value {
            serde_json::json!({
                "id": 100,
                "name": "CI",
                "status": "completed",
                "conclusion": "failure",
                "head_branch": "feat/test",
                "head_sha": "abc123def456",
                "html_url": "https://github.com/owner/repo/actions/runs/100",
                "run_started_at": "2024-01-01T00:00:00Z",
                "updated_at": "2024-01-01T00:01:00Z"
            })
        }

        fn sample_jobs_json() -> serde_json::Value {
            serde_json::json!({
                "jobs": [
                    {
                        "id": 201,
                        "name": "Build",
                        "status": "completed",
                        "conclusion": "success",
                        "html_url": "https://github.com/owner/repo/actions/runs/100/job/201",
                        "started_at": "2024-01-01T00:00:00Z",
                        "completed_at": "2024-01-01T00:00:30Z"
                    },
                    {
                        "id": 202,
                        "name": "Test",
                        "status": "completed",
                        "conclusion": "failure",
                        "html_url": "https://github.com/owner/repo/actions/runs/100/job/202",
                        "started_at": "2024-01-01T00:00:00Z",
                        "completed_at": "2024-01-01T00:00:45Z"
                    }
                ]
            })
        }

        #[tokio::test]
        async fn test_get_pipeline_by_branch() {
            let server = MockServer::start();

            // Mock: completed runs for branch
            server.mock(|when, then| {
                when.method(GET)
                    .path("/repos/owner/repo/actions/runs")
                    .query_param("branch", "main")
                    .query_param("status", "completed");
                then.status(200).json_body(serde_json::json!({
                    "workflow_runs": [sample_workflow_run_json()]
                }));
            });

            // Mock: in-progress runs (empty)
            server.mock(|when, then| {
                when.method(GET)
                    .path("/repos/owner/repo/actions/runs")
                    .query_param("status", "in_progress");
                then.status(200)
                    .json_body(serde_json::json!({ "workflow_runs": [] }));
            });

            // Mock: jobs
            server.mock(|when, then| {
                when.method(GET)
                    .path("/repos/owner/repo/actions/runs/100/jobs");
                then.status(200).json_body(sample_jobs_json());
            });

            // Mock: failed job log
            server.mock(|when, then| {
                when.method(GET)
                    .path("/repos/owner/repo/actions/jobs/202/logs");
                then.status(200)
                    .body("Step 1\nerror: test failed\nStep 3\n");
            });

            let client = create_test_client(&server);
            let input = devboy_core::GetPipelineInput {
                branch: Some("main".into()),
                mr_key: None,
                include_failed_logs: true,
            };

            let result = client.get_pipeline(input).await.unwrap();

            assert_eq!(result.id, "100");
            assert_eq!(result.status, PipelineStatus::Failed);
            assert_eq!(result.reference, "main");
            assert_eq!(result.summary.total, 2);
            assert_eq!(result.summary.success, 1);
            assert_eq!(result.summary.failed, 1);
            assert_eq!(result.stages.len(), 1);
            assert_eq!(result.stages[0].name, "CI");
            assert_eq!(result.stages[0].jobs.len(), 2);
            assert_eq!(result.failed_jobs.len(), 1);
            assert_eq!(result.failed_jobs[0].name, "Test");
            assert!(result.failed_jobs[0].error_snippet.is_some());
        }

        #[tokio::test]
        async fn test_get_pipeline_by_mr_key() {
            let server = MockServer::start();

            // Mock: get PR to resolve branch
            server.mock(|when, then| {
                when.method(GET).path("/repos/owner/repo/pulls/42");
                then.status(200).json_body(sample_pr_json());
            });

            // Mock: completed runs
            server.mock(|when, then| {
                when.method(GET)
                    .path("/repos/owner/repo/actions/runs")
                    .query_param("status", "completed");
                then.status(200).json_body(serde_json::json!({
                    "workflow_runs": [sample_workflow_run_json()]
                }));
            });

            // Mock: in-progress runs
            server.mock(|when, then| {
                when.method(GET)
                    .path("/repos/owner/repo/actions/runs")
                    .query_param("status", "in_progress");
                then.status(200)
                    .json_body(serde_json::json!({ "workflow_runs": [] }));
            });

            // Mock: jobs
            server.mock(|when, then| {
                when.method(GET)
                    .path("/repos/owner/repo/actions/runs/100/jobs");
                then.status(200).json_body(sample_jobs_json());
            });

            let client = create_test_client(&server);
            let input = devboy_core::GetPipelineInput {
                branch: None,
                mr_key: Some("pr#42".into()),
                include_failed_logs: false,
            };

            let result = client.get_pipeline(input).await.unwrap();
            assert_eq!(result.id, "100");
        }

        #[tokio::test]
        async fn test_get_job_logs_smart_mode() {
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(GET)
                    .path("/repos/owner/repo/actions/jobs/202/logs");
                then.status(200)
                    .body("Building...\nCompiling...\nerror: cannot find module 'foo'\nDone.\n");
            });

            let client = create_test_client(&server);
            let options = devboy_core::JobLogOptions {
                mode: devboy_core::JobLogMode::Smart,
            };

            let result = client.get_job_logs("202", options).await.unwrap();
            assert_eq!(result.job_id, "202");
            assert_eq!(result.mode, "smart");
            assert!(result.content.contains("cannot find module"));
        }

        #[tokio::test]
        async fn test_get_job_logs_search_mode() {
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(GET)
                    .path("/repos/owner/repo/actions/jobs/202/logs");
                then.status(200)
                    .body("Line 1\nLine 2\nERROR: something broke\nLine 4\nLine 5\n");
            });

            let client = create_test_client(&server);
            let options = devboy_core::JobLogOptions {
                mode: devboy_core::JobLogMode::Search {
                    pattern: "ERROR".into(),
                    context: 1,
                    max_matches: 5,
                },
            };

            let result = client.get_job_logs("202", options).await.unwrap();
            assert_eq!(result.mode, "search");
            assert!(result.content.contains("ERROR: something broke"));
            assert!(result.content.contains("Match at line 3"));
        }

        #[tokio::test]
        async fn test_get_job_logs_paginated_mode() {
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(GET)
                    .path("/repos/owner/repo/actions/jobs/202/logs");
                then.status(200)
                    .body("Line 1\nLine 2\nLine 3\nLine 4\nLine 5\n");
            });

            let client = create_test_client(&server);
            let options = devboy_core::JobLogOptions {
                mode: devboy_core::JobLogMode::Paginated {
                    offset: 1,
                    limit: 2,
                },
            };

            let result = client.get_job_logs("202", options).await.unwrap();
            assert_eq!(result.mode, "paginated");
            assert!(result.content.contains("Line 2"));
            assert!(result.content.contains("Line 3"));
            assert!(!result.content.contains("Line 1"));
            assert!(!result.content.contains("Line 4"));
        }

        // =================================================================
        // Attachment tests (Phase 2)
        // =================================================================

        #[tokio::test]
        async fn test_get_issue_attachments_parses_body_and_comments() {
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(GET).path("/repos/owner/repo/issues/42");
                then.status(200).json_body(serde_json::json!({
                    "id": 1,
                    "number": 42,
                    "title": "bug",
                    "body": "Error: ![screen](https://user-images.githubusercontent.com/1/screen.png)",
                    "state": "open",
                    "html_url": "https://github.com/owner/repo/issues/42",
                    "created_at": "2024-01-01T00:00:00Z",
                    "updated_at": "2024-01-02T00:00:00Z"
                }));
            });
            server.mock(|when, then| {
                when.method(GET)
                    .path("/repos/owner/repo/issues/42/comments");
                then.status(200).json_body(serde_json::json!([
                    {
                        "id": 10,
                        "body": "Log [here](https://user-images.githubusercontent.com/1/log.txt)",
                        "html_url": "https://github.com/owner/repo/issues/42#issuecomment-10",
                        "created_at": "2024-01-03T00:00:00Z",
                        "updated_at": "2024-01-03T00:00:00Z"
                    }
                ]));
            });

            let client = create_test_client(&server);
            let attachments = client.get_issue_attachments("gh#42").await.unwrap();
            assert_eq!(attachments.len(), 2);
            assert_eq!(attachments[0].filename, "screen");
            assert_eq!(attachments[1].filename, "here");
        }

        #[tokio::test]
        async fn test_download_attachment_fetches_url() {
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(GET).path("/cdn/file.txt");
                then.status(200).body("github-bytes");
            });

            let client = create_test_client(&server);
            let url = format!("{}/cdn/file.txt", server.base_url());
            let bytes = client.download_attachment("gh#42", &url).await.unwrap();
            assert_eq!(bytes, b"github-bytes");
        }

        #[tokio::test]
        async fn test_github_asset_capabilities() {
            let server = MockServer::start();
            let client = create_test_client(&server);
            let caps = client.asset_capabilities();
            assert!(!caps.issue.upload, "GitHub has no public upload API");
            assert!(caps.issue.download);
            assert!(caps.issue.list);
            assert!(!caps.issue.delete);
            assert!(!caps.merge_request.upload);
            assert!(caps.merge_request.download);
        }
    }

    // =========================================================================
    // Pipeline utility unit tests
    // =========================================================================

    #[test]
    fn test_map_gh_status() {
        assert_eq!(
            map_gh_status(Some("completed"), Some("success")),
            PipelineStatus::Success
        );
        assert_eq!(
            map_gh_status(Some("completed"), Some("failure")),
            PipelineStatus::Failed
        );
        assert_eq!(
            map_gh_status(Some("in_progress"), None),
            PipelineStatus::Running
        );
        assert_eq!(map_gh_status(Some("queued"), None), PipelineStatus::Pending);
        assert_eq!(
            map_gh_status(Some("completed"), Some("cancelled")),
            PipelineStatus::Canceled
        );
        assert_eq!(map_gh_status(None, None), PipelineStatus::Unknown);
    }

    #[test]
    fn test_strip_ansi() {
        assert_eq!(strip_ansi("\x1b[31merror\x1b[0m"), "error");
        assert_eq!(strip_ansi("no ansi here"), "no ansi here");
        assert_eq!(strip_ansi("\x1b[1m\x1b[32mgreen\x1b[0m"), "green");
    }

    #[test]
    fn test_extract_errors_finds_patterns() {
        let log = "Step 1: build\nStep 2: test\nerror: test failed at line 42\nStep 4: done\n";
        let result = extract_errors(log, 10).unwrap();
        assert!(result.contains("error: test failed"));
    }

    #[test]
    fn test_extract_errors_fallback_to_tail() {
        let log = "Line 1\nLine 2\nLine 3\n";
        let result = extract_errors(log, 10).unwrap();
        assert!(result.contains("Line 3"));
    }

    #[test]
    fn test_extract_errors_empty_log() {
        assert!(extract_errors("", 10).is_none());
    }

    #[test]
    fn test_estimate_duration() {
        let d = estimate_duration(Some("2024-01-01T00:00:00Z"), Some("2024-01-01T00:01:30Z"));
        assert_eq!(d, Some(90));
    }

    #[test]
    fn test_estimate_duration_invalid() {
        assert!(estimate_duration(None, Some("2024-01-01T00:00:00Z")).is_none());
        assert!(estimate_duration(Some("not-a-date"), Some("2024-01-01T00:00:00Z")).is_none());
    }
}
