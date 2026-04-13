//! GitLab API client implementation.

use async_trait::async_trait;
use devboy_core::{
    AssetCapabilities, AssetMeta, CodePosition, Comment, ContextCapabilities, CreateCommentInput,
    CreateIssueInput, CreateMergeRequestInput, Discussion, Error, FailedJob, FileDiff,
    GetPipelineInput, Issue, IssueFilter, IssueProvider, JobLogMode, JobLogOptions, JobLogOutput,
    MergeRequest, MergeRequestProvider, MrFilter, PipelineInfo, PipelineJob, PipelineProvider,
    PipelineStage, PipelineStatus, PipelineSummary, Provider, ProviderResult, Result,
    UpdateIssueInput, User, parse_markdown_attachments,
};
use tracing::{debug, warn};

use crate::DEFAULT_GITLAB_URL;
use crate::types::{
    CreateDiscussionRequest, CreateIssueRequest, CreateMergeRequestRequest, CreateNoteRequest,
    DiscussionPosition, GitLabDiff, GitLabDiscussion, GitLabIssue, GitLabMergeRequest,
    GitLabMergeRequestChanges, GitLabNote, GitLabNotePosition, GitLabUser, UpdateIssueRequest,
};

/// GitLab API client.
pub struct GitLabClient {
    base_url: String,
    project_id: String,
    token: String,
    proxy_headers: Option<std::collections::HashMap<String, String>>,
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
            proxy_headers: None,
            client: reqwest::Client::new(),
        }
    }

    /// Configure proxy mode with extra headers added to every request.
    /// When proxy is active, the provider's own auth header (`PRIVATE-TOKEN`)
    /// is suppressed — the proxy handles authentication.
    pub fn with_proxy(mut self, headers: std::collections::HashMap<String, String>) -> Self {
        self.proxy_headers = Some(headers);
        self
    }

    /// Build request with auth headers.
    ///
    /// When proxy is configured, provider's own auth is suppressed and
    /// proxy headers are added instead. The proxy handles authentication.
    fn request(&self, method: reqwest::Method, url: &str) -> reqwest::RequestBuilder {
        let mut req = self.client.request(method, url);
        if let Some(headers) = &self.proxy_headers {
            for (key, value) in headers {
                req = req.header(key.as_str(), value.as_str());
            }
        } else {
            req = req.header("PRIVATE-TOKEN", &self.token);
        }
        req
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

    /// Make an authenticated GET request and extract pagination from headers.
    async fn get_with_pagination<T: serde::de::DeserializeOwned>(
        &self,
        url: &str,
        filter_offset: Option<u32>,
        filter_limit: Option<u32>,
    ) -> Result<(T, Option<devboy_core::Pagination>)> {
        debug!(url = url, "GitLab GET request (with pagination)");

        let response = self
            .request(reqwest::Method::GET, url)
            .send()
            .await
            .map_err(|e| Error::Http(e.to_string()))?;

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

        // Extract pagination from GitLab headers
        let pagination = Self::extract_pagination(&response, filter_offset, filter_limit);

        let data: T = response
            .json()
            .await
            .map_err(|e| Error::InvalidData(format!("Failed to parse response: {}", e)))?;

        Ok((data, pagination))
    }

    /// Extract pagination metadata from GitLab response headers.
    fn extract_pagination(
        response: &reqwest::Response,
        offset: Option<u32>,
        limit: Option<u32>,
    ) -> Option<devboy_core::Pagination> {
        let headers = response.headers();

        let x_total = headers
            .get("x-total")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u32>().ok());

        let x_page = headers
            .get("x-page")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u32>().ok());

        let x_total_pages = headers
            .get("x-total-pages")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u32>().ok());

        let limit = limit.unwrap_or(20);
        let offset = offset.unwrap_or(0);

        let has_more = match (x_page, x_total_pages) {
            (Some(page), Some(total_pages)) => page < total_pages,
            _ => false,
        };

        Some(devboy_core::Pagination {
            offset,
            limit,
            total: x_total,
            has_more,
        })
    }

    /// Upload a raw file to the project's shared uploads bucket and
    /// return an absolute download URL for the uploaded blob.
    ///
    /// GitLab does not expose a per-issue attachment API — instead all
    /// uploads share a project-wide `/projects/{id}/uploads` endpoint and
    /// get embedded into issue / MR / note bodies via a markdown snippet
    /// that links back to that URL.
    async fn upload_project_file(&self, filename: &str, data: &[u8]) -> Result<String> {
        let url = self.project_url("/uploads");

        let part = reqwest::multipart::Part::bytes(data.to_vec())
            .file_name(filename.to_string())
            .mime_str("application/octet-stream")
            .map_err(|e| Error::Http(format!("failed to build multipart: {e}")))?;
        let form = reqwest::multipart::Form::new().part("file", part);

        let response = self
            .request(reqwest::Method::POST, &url)
            .multipart(form)
            .send()
            .await
            .map_err(|e| Error::Http(e.to_string()))?;

        let status = response.status();
        if !status.is_success() {
            let message = response.text().await.unwrap_or_default();
            return Err(Error::from_status(status.as_u16(), message));
        }

        let body: serde_json::Value = response
            .json()
            .await
            .map_err(|e| Error::InvalidData(format!("failed to parse upload response: {e}")))?;

        // GitLab returns: { "alt": "screen", "url": "/uploads/<hash>/screen.png",
        //                   "full_path": "/namespace/project/uploads/<hash>/screen.png",
        //                   "markdown": "![screen](/uploads/...)" }
        let relative = body
            .get("full_path")
            .or_else(|| body.get("url"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                Error::InvalidData(
                    "GitLab upload response contains no usable url or full_path".to_string(),
                )
            })?;
        Ok(absolutize_gitlab_url(&self.base_url, relative))
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

    /// Download a URL that is expected to belong to this GitLab instance.
    ///
    /// Auth headers (`PRIVATE-TOKEN` / proxy headers) are only sent when
    /// the URL host matches the configured `base_url`. For cross-origin
    /// URLs (which can appear in markdown via user-supplied links) the
    /// request is made anonymously to prevent token leakage.
    async fn download_trusted_url(&self, url: &str) -> Result<Vec<u8>> {
        let request = if is_same_origin(&self.base_url, url) {
            self.request(reqwest::Method::GET, url)
        } else {
            tracing::warn!(
                url,
                "downloading cross-origin attachment without auth headers"
            );
            self.client.get(url)
        };
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
}

/// Check whether a URL belongs to the same origin (scheme + host) as
/// the configured base URL. Relative URLs and paths always count as
/// same-origin. Scheme-relative `//host/...` URLs are treated as
/// cross-origin to avoid ambiguity.
///
/// This prevents sending auth headers (PRIVATE-TOKEN / proxy) over
/// plaintext HTTP when the base URL uses HTTPS.
fn is_same_origin(base_url: &str, url: &str) -> bool {
    if !url.contains("://") && !url.starts_with("//") {
        return true; // relative path
    }
    let (base_scheme, base_host) = split_scheme_host(base_url);
    let (url_scheme, url_host) = split_scheme_host(url);

    base_scheme.eq_ignore_ascii_case(&url_scheme) && base_host.eq_ignore_ascii_case(&url_host)
}

/// Extract (scheme, host) from a URL string. Returns empty strings for
/// components that cannot be parsed.
fn split_scheme_host(url: &str) -> (String, String) {
    let (scheme, rest) = match url.split_once("://") {
        Some((s, r)) => (s.to_ascii_lowercase(), r),
        None => return (String::new(), String::new()),
    };
    let host = rest.split('/').next().unwrap_or("").to_ascii_lowercase();
    (scheme, host)
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
        parent: None,
        subtasks: vec![],
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
    async fn get_issues(&self, filter: IssueFilter) -> Result<ProviderResult<Issue>> {
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

        if let Some(labels) = &filter.labels
            && !labels.is_empty()
        {
            params.push(format!("labels={}", labels.join(",")));
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

        let (gl_issues, pagination): (Vec<GitLabIssue>, _) = self
            .get_with_pagination(&url, filter.offset, filter.limit)
            .await?;
        let issues: Vec<Issue> = gl_issues.iter().map(map_issue).collect();
        let mut result = ProviderResult::new(issues);
        result.pagination = pagination;
        result.sort_info = Some(devboy_core::SortInfo {
            sort_by: Some(filter.sort_by.as_deref().unwrap_or("updated_at").into()),
            sort_order: match filter.sort_order.as_deref() {
                Some("asc") => devboy_core::SortOrder::Asc,
                _ => devboy_core::SortOrder::Desc,
            },
            available_sorts: vec!["created_at".into(), "updated_at".into()],
        });
        Ok(result)
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

    async fn get_comments(&self, issue_key: &str) -> Result<ProviderResult<Comment>> {
        let iid = parse_issue_key(issue_key)?;
        let url = self.project_url(&format!("/issues/{}/notes", iid));
        let gl_notes: Vec<GitLabNote> = self.get(&url).await?;

        // Filter out system notes
        let comments: Vec<Comment> = gl_notes
            .iter()
            .filter(|n| !n.system)
            .map(map_note)
            .collect();
        Ok(comments.into())
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

    async fn upload_attachment(
        &self,
        _issue_key: &str,
        filename: &str,
        data: &[u8],
    ) -> Result<String> {
        // GitLab has no per-issue attachment endpoint. Instead we upload to
        // the project's shared uploads bucket and return the absolute URL
        // that can be embedded into any issue / MR / note body.
        self.upload_project_file(filename, data).await
    }

    async fn get_issue_attachments(&self, issue_key: &str) -> Result<Vec<AssetMeta>> {
        // GitLab does not expose an attachment listing — we reconstruct it
        // from the markdown of the issue body and all comments.
        let issue = self.get_issue(issue_key).await?;
        let comments = self.get_comments(issue_key).await?;

        let mut attachments: Vec<AssetMeta> = Vec::new();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

        let mut collect = |source: &str| {
            for att in parse_markdown_attachments(source) {
                if seen.insert(att.url.clone()) {
                    attachments.push(markdown_to_meta(&att, &self.base_url));
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
        let absolute = absolutize_gitlab_url(&self.base_url, asset_id);
        self.download_trusted_url(&absolute).await
    }

    fn asset_capabilities(&self) -> AssetCapabilities {
        // GitLab project uploads are immutable, so `delete` stays false.
        let caps = ContextCapabilities {
            upload: true,
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
        "gitlab"
    }
}

#[async_trait]
impl MergeRequestProvider for GitLabClient {
    async fn get_merge_requests(&self, filter: MrFilter) -> Result<ProviderResult<MergeRequest>> {
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

        if let Some(labels) = &filter.labels
            && !labels.is_empty()
        {
            params.push(format!("labels={}", labels.join(",")));
        }

        if let Some(limit) = filter.limit {
            params.push(format!("per_page={}", limit.min(100)));
        }

        let order_by = filter.sort_by.as_deref().unwrap_or("updated_at");
        let sort_order = filter.sort_order.as_deref().unwrap_or("desc");
        params.push(format!("order_by={}", order_by));
        params.push(format!("sort={}", sort_order));

        if let Some(offset) = filter.offset {
            let page = (offset / filter.limit.unwrap_or(20)) + 1;
            params.push(format!("page={}", page));
        }

        if !params.is_empty() {
            url.push_str(&format!("?{}", params.join("&")));
        }

        let (gl_mrs, pagination): (Vec<GitLabMergeRequest>, _) = self
            .get_with_pagination(&url, filter.offset, filter.limit)
            .await?;
        let mrs: Vec<MergeRequest> = gl_mrs.iter().map(map_merge_request).collect();
        let mut result = ProviderResult::new(mrs);
        result.pagination = pagination;
        result.sort_info = Some(devboy_core::SortInfo {
            sort_by: Some(order_by.into()),
            sort_order: match sort_order {
                "asc" => devboy_core::SortOrder::Asc,
                _ => devboy_core::SortOrder::Desc,
            },
            available_sorts: vec!["created_at".into(), "updated_at".into()],
        });
        Ok(result)
    }

    async fn get_merge_request(&self, key: &str) -> Result<MergeRequest> {
        let iid = parse_mr_key(key)?;
        let url = self.project_url(&format!("/merge_requests/{}", iid));
        let gl_mr: GitLabMergeRequest = self.get(&url).await?;
        Ok(map_merge_request(&gl_mr))
    }

    async fn get_discussions(&self, mr_key: &str) -> Result<ProviderResult<Discussion>> {
        let iid = parse_mr_key(mr_key)?;
        let url = self.project_url(&format!("/merge_requests/{}/discussions", iid));
        let gl_discussions: Vec<GitLabDiscussion> = self.get(&url).await?;

        // Map and filter out empty discussions (all system notes)
        let discussions: Vec<Discussion> = gl_discussions
            .iter()
            .map(map_discussion)
            .filter(|d| !d.comments.is_empty())
            .collect();
        Ok(discussions.into())
    }

    async fn get_diffs(&self, mr_key: &str) -> Result<ProviderResult<FileDiff>> {
        let iid = parse_mr_key(mr_key)?;
        // Use the changes endpoint which returns diffs with content
        let url = self.project_url(&format!("/merge_requests/{}/changes", iid));
        let gl_changes: GitLabMergeRequestChanges = self.get(&url).await?;
        Ok(gl_changes
            .changes
            .iter()
            .map(map_diff)
            .collect::<Vec<_>>()
            .into())
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

    async fn create_merge_request(&self, input: CreateMergeRequestInput) -> Result<MergeRequest> {
        let url = self.project_url("/merge_requests");

        let labels = if input.labels.is_empty() {
            None
        } else {
            Some(input.labels.join(","))
        };

        // Prefix title with "Draft: " if draft is requested
        let title = if input.draft && !input.title.starts_with("Draft:") {
            format!("Draft: {}", input.title)
        } else {
            input.title
        };

        if !input.reviewers.is_empty() {
            warn!(
                "GitLab reviewers require user IDs, not usernames; ignoring reviewers: {:?}",
                input.reviewers
            );
        }

        let request = CreateMergeRequestRequest {
            source_branch: input.source_branch,
            target_branch: input.target_branch,
            title,
            description: input.description,
            labels,
            reviewer_ids: None,
        };

        let gl_mr: GitLabMergeRequest = self.post(&url, &request).await?;
        Ok(map_merge_request(&gl_mr))
    }

    async fn get_mr_attachments(&self, mr_key: &str) -> Result<Vec<AssetMeta>> {
        let mr = self.get_merge_request(mr_key).await?;
        let discussions = self.get_discussions(mr_key).await?;

        let mut attachments: Vec<AssetMeta> = Vec::new();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

        let mut collect = |source: &str| {
            for att in parse_markdown_attachments(source) {
                if seen.insert(att.url.clone()) {
                    attachments.push(markdown_to_meta(&att, &self.base_url));
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
        let absolute = absolutize_gitlab_url(&self.base_url, asset_id);
        self.download_trusted_url(&absolute).await
    }

    fn provider_name(&self) -> &'static str {
        "gitlab"
    }
}

/// Convert a relative GitLab upload path to an absolute URL.
///
/// Pass-through for URLs that already contain a scheme.
fn absolutize_gitlab_url(base: &str, url_or_path: &str) -> String {
    if url_or_path.starts_with("http://") || url_or_path.starts_with("https://") {
        return url_or_path.to_string();
    }
    let base = base.trim_end_matches('/');
    if url_or_path.starts_with('/') {
        format!("{base}{url_or_path}")
    } else {
        format!("{base}/{url_or_path}")
    }
}

/// Convert a parsed markdown attachment into an [`AssetMeta`] record.
fn markdown_to_meta(att: &devboy_core::MarkdownAttachment, base_url: &str) -> AssetMeta {
    let absolute = absolutize_gitlab_url(base_url, &att.url);
    AssetMeta {
        // For GitLab there's no stable attachment id — the URL doubles as
        // both the lookup key and the download target.
        id: att.url.clone(),
        filename: att.filename.clone(),
        mime_type: None,
        size: None,
        url: Some(absolute),
        created_at: None,
        author: None,
        cached: false,
        local_path: None,
        checksum_sha256: None,
        analysis: None,
    }
}

// =============================================================================
// Pipeline Provider (GitLab Pipelines API)
// =============================================================================

/// GitLab pipeline.
#[derive(Debug, serde::Deserialize)]
struct GlPipeline {
    id: u64,
    status: String,
    #[serde(rename = "ref")]
    ref_name: String,
    sha: String,
    web_url: Option<String>,
    duration: Option<u64>,
    coverage: Option<String>,
}

/// GitLab pipeline job.
#[derive(Debug, serde::Deserialize)]
struct GlJob {
    id: u64,
    name: String,
    status: String,
    stage: String,
    web_url: Option<String>,
    duration: Option<f64>,
}

fn map_gl_pipeline_status(status: &str) -> PipelineStatus {
    match status {
        "success" => PipelineStatus::Success,
        "failed" => PipelineStatus::Failed,
        "running" => PipelineStatus::Running,
        "pending" | "waiting_for_resource" | "preparing" => PipelineStatus::Pending,
        "canceled" => PipelineStatus::Canceled,
        "skipped" => PipelineStatus::Skipped,
        "manual" => PipelineStatus::Pending,
        _ => PipelineStatus::Unknown,
    }
}

/// Strip ANSI escape codes from text.
fn strip_ansi(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
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

/// Extract GitLab section content from log.
#[allow(dead_code)]
fn extract_section(log: &str, section_name: &str) -> Option<String> {
    let start_marker = "section_start:";
    let end_marker = "section_end:";
    let lines: Vec<&str> = log.lines().collect();
    let mut in_section = false;
    let mut section_lines = Vec::new();

    for line in &lines {
        let stripped = strip_ansi(line);
        if stripped.contains(start_marker) && stripped.contains(section_name) {
            in_section = true;
            continue;
        }
        if stripped.contains(end_marker) && stripped.contains(section_name) {
            break;
        }
        if in_section {
            section_lines.push(strip_ansi(line).trim().to_string());
        }
    }

    if section_lines.is_empty() {
        None
    } else {
        Some(section_lines.join("\n"))
    }
}

/// List available sections in a GitLab job log.
#[allow(dead_code)]
fn list_sections(log: &str) -> Vec<String> {
    let mut sections = Vec::new();
    for line in log.lines() {
        let stripped = strip_ansi(line);
        if let Some(pos) = stripped.find("section_start:") {
            // Format: section_start:TIMESTAMP:SECTION_NAME\r...
            let after = &stripped[pos + "section_start:".len()..];
            if let Some(colon_pos) = after.find(':') {
                let name_part = &after[colon_pos + 1..];
                let name = name_part
                    .split(['\r', '\n', '\x1b'])
                    .next()
                    .unwrap_or("")
                    .to_string();
                if !name.is_empty() && !sections.contains(&name) {
                    sections.push(name);
                }
            }
        }
    }
    sections
}

#[async_trait]
impl PipelineProvider for GitLabClient {
    fn provider_name(&self) -> &'static str {
        "gitlab"
    }

    async fn get_pipeline(&self, input: GetPipelineInput) -> Result<PipelineInfo> {
        // Resolve pipeline
        let pipeline: GlPipeline = if let Some(ref mr_key) = input.mr_key {
            // MR pipeline: GET /projects/:id/merge_requests/:iid/pipelines
            let iid = parse_mr_key(mr_key)?;
            let url = self.project_url(&format!("/merge_requests/{iid}/pipelines?per_page=1"));
            let pipelines: Vec<GlPipeline> = self.get(&url).await?;
            pipelines
                .into_iter()
                .next()
                .ok_or_else(|| Error::NotFound(format!("No pipeline found for MR !{iid}")))?
        } else {
            let ref_name = input.branch.as_deref().unwrap_or("main");
            // Branch pipeline: GET /projects/:id/pipelines?ref=BRANCH&per_page=1
            let url = self.project_url(&format!(
                "/pipelines?ref={}&per_page=1&order_by=id&sort=desc",
                urlencoding::encode(ref_name)
            ));
            let pipelines: Vec<GlPipeline> = self.get(&url).await?;

            if let Some(p) = pipelines.into_iter().next() {
                p
            } else {
                // Fallback: try MR pipeline for this branch
                let mrs_url = self.project_url(&format!(
                    "/merge_requests?source_branch={}&state=opened&per_page=1",
                    urlencoding::encode(ref_name)
                ));
                let mrs: Vec<GitLabMergeRequest> = self.get(&mrs_url).await?;
                if let Some(mr) = mrs.first() {
                    let mr_pipes_url = self
                        .project_url(&format!("/merge_requests/{}/pipelines?per_page=1", mr.iid));
                    let mr_pipelines: Vec<GlPipeline> = self.get(&mr_pipes_url).await?;
                    mr_pipelines.into_iter().next().ok_or_else(|| {
                        Error::NotFound(format!("No pipeline found for branch '{ref_name}'"))
                    })?
                } else {
                    return Err(Error::NotFound(format!(
                        "No pipeline found for branch '{ref_name}'"
                    )));
                }
            }
        };

        // Get jobs for pipeline
        let jobs_url = self.project_url(&format!("/pipelines/{}/jobs?per_page=100", pipeline.id));
        let gl_jobs: Vec<GlJob> = self.get(&jobs_url).await?;

        // Build summary and group by stage
        let mut summary = PipelineSummary {
            total: gl_jobs.len() as u32,
            ..Default::default()
        };

        let mut stages_map: std::collections::BTreeMap<String, Vec<PipelineJob>> =
            std::collections::BTreeMap::new();
        let mut failed_job_ids: Vec<(u64, String)> = Vec::new();

        for job in &gl_jobs {
            let status = map_gl_pipeline_status(&job.status);
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

            stages_map
                .entry(job.stage.clone())
                .or_default()
                .push(PipelineJob {
                    id: job.id.to_string(),
                    name: job.name.clone(),
                    status,
                    url: job.web_url.clone(),
                    duration: job.duration.map(|d| d as u64),
                });
        }

        let stages: Vec<PipelineStage> = stages_map
            .into_iter()
            .map(|(name, jobs)| PipelineStage { name, jobs })
            .collect();

        // Fetch error snippets for failed jobs (max 5)
        let mut failed_jobs: Vec<FailedJob> = Vec::new();
        if input.include_failed_logs {
            for (job_id, job_name) in failed_job_ids.iter().take(5) {
                let trace_url = self.project_url(&format!("/jobs/{job_id}/trace"));
                let error_snippet =
                    match self.request(reqwest::Method::GET, &trace_url).send().await {
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

        let coverage = pipeline.coverage.and_then(|c| c.parse::<f64>().ok());

        Ok(PipelineInfo {
            id: pipeline.id.to_string(),
            status: map_gl_pipeline_status(&pipeline.status),
            reference: pipeline.ref_name,
            sha: pipeline.sha,
            url: pipeline.web_url,
            duration: pipeline.duration,
            coverage,
            summary,
            stages,
            failed_jobs,
        })
    }

    async fn get_job_logs(&self, job_id: &str, options: JobLogOptions) -> Result<JobLogOutput> {
        let trace_url = self.project_url(&format!("/jobs/{job_id}/trace"));
        let resp = self
            .request(reqwest::Method::GET, &trace_url)
            .send()
            .await
            .map_err(|e| Error::Network(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(Error::from_status(
                resp.status().as_u16(),
                format!("Failed to fetch job logs for job {job_id}"),
            ));
        }

        let raw_log = resp
            .text()
            .await
            .map_err(|e| Error::Network(e.to_string()))?;
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
                .unwrap()
                .items;

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
                    ..Default::default()
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
                .unwrap()
                .items;

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
            let discussions = client.get_discussions("mr#50").await.unwrap().items;

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
            let diffs = client.get_diffs("mr#50").await.unwrap().items;

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

        // =====================================================================
        // Pipeline tests
        // =====================================================================

        #[tokio::test]
        async fn test_get_pipeline_by_branch() {
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(GET)
                    .path("/api/v4/projects/123/pipelines")
                    .query_param("ref", "main");
                then.status(200).json_body(serde_json::json!([{
                    "id": 500,
                    "status": "failed",
                    "ref": "main",
                    "sha": "abc123",
                    "web_url": "https://gitlab.com/project/-/pipelines/500",
                    "duration": 120,
                    "coverage": "85.5"
                }]));
            });

            server.mock(|when, then| {
                when.method(GET)
                    .path("/api/v4/projects/123/pipelines/500/jobs");
                then.status(200).json_body(serde_json::json!([
                    {
                        "id": 601,
                        "name": "build",
                        "status": "success",
                        "stage": "build",
                        "web_url": "https://gitlab.com/project/-/jobs/601",
                        "duration": 30.0
                    },
                    {
                        "id": 602,
                        "name": "test",
                        "status": "failed",
                        "stage": "test",
                        "web_url": "https://gitlab.com/project/-/jobs/602",
                        "duration": 90.0
                    }
                ]));
            });

            server.mock(|when, then| {
                when.method(GET).path("/api/v4/projects/123/jobs/602/trace");
                then.status(200)
                    .body("Running tests...\nerror: assertion failed\nDone.\n");
            });

            let client = create_test_client(&server);
            let input = devboy_core::GetPipelineInput {
                branch: Some("main".into()),
                mr_key: None,
                include_failed_logs: true,
            };

            let result = client.get_pipeline(input).await.unwrap();

            assert_eq!(result.id, "500");
            assert_eq!(result.status, PipelineStatus::Failed);
            assert_eq!(result.reference, "main");
            assert_eq!(result.duration, Some(120));
            assert_eq!(result.coverage, Some(85.5));
            assert_eq!(result.summary.total, 2);
            assert_eq!(result.summary.success, 1);
            assert_eq!(result.summary.failed, 1);
            assert_eq!(result.stages.len(), 2); // build + test
            assert_eq!(result.failed_jobs.len(), 1);
            assert_eq!(result.failed_jobs[0].name, "test");
            assert!(
                result.failed_jobs[0]
                    .error_snippet
                    .as_ref()
                    .unwrap()
                    .contains("assertion failed")
            );
        }

        #[tokio::test]
        async fn test_get_pipeline_by_mr_key() {
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(GET)
                    .path("/api/v4/projects/123/merge_requests/42/pipelines");
                then.status(200).json_body(serde_json::json!([{
                    "id": 501,
                    "status": "success",
                    "ref": "feat/test",
                    "sha": "def456",
                    "web_url": null,
                    "duration": 60,
                    "coverage": null
                }]));
            });

            server.mock(|when, then| {
                when.method(GET)
                    .path("/api/v4/projects/123/pipelines/501/jobs");
                then.status(200).json_body(serde_json::json!([{
                    "id": 701,
                    "name": "lint",
                    "status": "success",
                    "stage": "verify",
                    "duration": 15.0
                }]));
            });

            let client = create_test_client(&server);
            let input = devboy_core::GetPipelineInput {
                branch: None,
                mr_key: Some("mr#42".into()),
                include_failed_logs: false,
            };

            let result = client.get_pipeline(input).await.unwrap();
            assert_eq!(result.id, "501");
            assert_eq!(result.status, PipelineStatus::Success);
            assert_eq!(result.summary.total, 1);
            assert_eq!(result.summary.success, 1);
        }

        #[tokio::test]
        async fn test_get_job_logs_smart() {
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(GET)
                    .path("/api/v4/projects/123/jobs/602/trace");
                then.status(200)
                    .body("Step 1\nStep 2\nerror[E0308]: mismatched types\n  --> src/main.rs:10\nStep 5\n");
            });

            let client = create_test_client(&server);
            let options = devboy_core::JobLogOptions {
                mode: devboy_core::JobLogMode::Smart,
            };

            let result = client.get_job_logs("602", options).await.unwrap();
            assert_eq!(result.mode, "smart");
            assert!(result.content.contains("mismatched types"));
        }

        #[tokio::test]
        async fn test_get_job_logs_search() {
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(GET).path("/api/v4/projects/123/jobs/602/trace");
                then.status(200)
                    .body("Line 1\nLine 2\nFAILED: test_foo\nLine 4\n");
            });

            let client = create_test_client(&server);
            let options = devboy_core::JobLogOptions {
                mode: devboy_core::JobLogMode::Search {
                    pattern: "FAILED".into(),
                    context: 1,
                    max_matches: 5,
                },
            };

            let result = client.get_job_logs("602", options).await.unwrap();
            assert_eq!(result.mode, "search");
            assert!(result.content.contains("FAILED: test_foo"));
        }

        #[tokio::test]
        async fn test_get_job_logs_paginated() {
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(GET).path("/api/v4/projects/123/jobs/602/trace");
                then.status(200).body("L1\nL2\nL3\nL4\nL5\n");
            });

            let client = create_test_client(&server);
            let options = devboy_core::JobLogOptions {
                mode: devboy_core::JobLogMode::Paginated {
                    offset: 2,
                    limit: 2,
                },
            };

            let result = client.get_job_logs("602", options).await.unwrap();
            assert_eq!(result.mode, "paginated");
            assert!(result.content.contains("L3"));
            assert!(result.content.contains("L4"));
            assert!(!result.content.contains("L1"));
        }

        // =================================================================
        // Attachment tests (Phase 2)
        // =================================================================

        #[tokio::test]
        async fn test_upload_attachment_returns_absolute_url() {
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(POST).path("/api/v4/projects/123/uploads");
                then.status(201).json_body(serde_json::json!({
                    "alt": "screen",
                    "url": "/uploads/abc/screen.png",
                    "full_path": "/ns/proj/uploads/abc/screen.png",
                    "markdown": "![screen](/uploads/abc/screen.png)"
                }));
            });

            let client = create_test_client(&server);
            let url = client
                .upload_attachment("gitlab#42", "screen.png", b"data")
                .await
                .unwrap();
            assert!(url.starts_with(&server.base_url()));
            assert!(url.contains("/uploads/abc/screen.png"));
        }

        #[tokio::test]
        async fn test_get_issue_attachments_parses_body_and_notes() {
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(GET).path("/api/v4/projects/123/issues/42");
                then.status(200).json_body(serde_json::json!({
                    "id": 1,
                    "iid": 42,
                    "title": "bug",
                    "description": "See ![screen](/uploads/hash1/screen.png)",
                    "state": "opened",
                    "web_url": "https://example/gl/ns/proj/-/issues/42",
                    "created_at": "2024-01-01T00:00:00Z",
                    "updated_at": "2024-01-02T00:00:00Z"
                }));
            });
            server.mock(|when, then| {
                when.method(GET)
                    .path("/api/v4/projects/123/issues/42/notes");
                then.status(200).json_body(serde_json::json!([
                    {
                        "id": 10,
                        "body": "Also [log](/uploads/hash2/trace.log)",
                        "system": false,
                        "created_at": "2024-01-01T00:00:00Z"
                    },
                    {
                        "id": 11,
                        "body": "Duplicate ![screen](/uploads/hash1/screen.png)",
                        "system": false,
                        "created_at": "2024-01-02T00:00:00Z"
                    }
                ]));
            });

            let client = create_test_client(&server);
            let attachments = client.get_issue_attachments("gitlab#42").await.unwrap();
            assert_eq!(attachments.len(), 2, "duplicates should be dropped");
            assert_eq!(attachments[0].filename, "screen");
            assert!(
                attachments[0]
                    .url
                    .as_deref()
                    .unwrap()
                    .contains("/uploads/hash1/screen.png")
            );
            assert_eq!(attachments[1].filename, "log");
        }

        #[tokio::test]
        async fn test_download_attachment_relative_path() {
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(GET).path("/uploads/hash/file.txt");
                then.status(200).body("hello");
            });

            let client = create_test_client(&server);
            let bytes = client
                .download_attachment("gitlab#42", "/uploads/hash/file.txt")
                .await
                .unwrap();
            assert_eq!(bytes, b"hello");
        }

        #[tokio::test]
        async fn test_gitlab_asset_capabilities() {
            let server = MockServer::start();
            let client = create_test_client(&server);
            let caps = client.asset_capabilities();
            assert!(caps.issue.upload);
            assert!(caps.issue.download);
            assert!(caps.issue.list);
            assert!(!caps.issue.delete);
            // GitLab uploads are shared, so MR caps match issue caps.
            assert!(caps.merge_request.upload);
            assert!(caps.merge_request.list);
        }
    }

    // =========================================================================
    // Pipeline utility unit tests
    // =========================================================================

    #[test]
    fn test_map_gl_pipeline_status() {
        assert_eq!(map_gl_pipeline_status("success"), PipelineStatus::Success);
        assert_eq!(map_gl_pipeline_status("failed"), PipelineStatus::Failed);
        assert_eq!(map_gl_pipeline_status("running"), PipelineStatus::Running);
        assert_eq!(map_gl_pipeline_status("pending"), PipelineStatus::Pending);
        assert_eq!(map_gl_pipeline_status("canceled"), PipelineStatus::Canceled);
        assert_eq!(map_gl_pipeline_status("skipped"), PipelineStatus::Skipped);
        assert_eq!(map_gl_pipeline_status("manual"), PipelineStatus::Pending);
        assert_eq!(map_gl_pipeline_status("unknown"), PipelineStatus::Unknown);
    }

    #[test]
    fn test_strip_ansi_gitlab() {
        assert_eq!(strip_ansi("\x1b[0K\x1b[32;1mRunning\x1b[0m"), "Running");
        assert_eq!(strip_ansi("plain text"), "plain text");
    }

    #[test]
    fn test_extract_errors_gitlab() {
        let log = "section_start:build\nCompiling...\nerror: build failed\nsection_end:build\n";
        let result = extract_errors(log, 10).unwrap();
        assert!(result.contains("build failed"));
    }

    #[test]
    fn test_extract_section() {
        let log = "before\nsection_start:1234:build_script\ncompiling...\ndone\nsection_end:1234:build_script\nafter\n";
        let result = extract_section(log, "build_script").unwrap();
        assert!(result.contains("compiling"));
        assert!(result.contains("done"));
        assert!(!result.contains("before"));
        assert!(!result.contains("after"));
    }

    #[test]
    fn test_list_sections() {
        let log = "section_start:111:prepare_script\nstuff\nsection_end:111:prepare_script\nsection_start:222:build_script\nmore\nsection_end:222:build_script\n";
        let sections = list_sections(log);
        assert!(sections.contains(&"prepare_script".to_string()));
        assert!(sections.contains(&"build_script".to_string()));
    }
}
