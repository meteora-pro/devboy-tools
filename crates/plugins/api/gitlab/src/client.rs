//! GitLab API client implementation.

use async_trait::async_trait;
use devboy_core::{
    Comment, CreateCommentInput, CreateIssueInput, Discussion, Error, FileDiff, Issue, IssueFilter,
    IssueProvider, MergeRequest, MergeRequestProvider, MrFilter, Provider, Result,
    UpdateIssueInput, User,
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

    /// Get the API URL for a given endpoint.
    fn api_url(&self, endpoint: &str) -> String {
        format!("{}/api/v4{}", self.base_url, endpoint)
    }

    /// Make an authenticated GET request.
    async fn get(&self, url: &str) -> Result<reqwest::Response> {
        let response = self
            .client
            .get(url)
            .header("PRIVATE-TOKEN", &self.token)
            .send()
            .await
            .map_err(|e| Error::Http(e.to_string()))?;

        if !response.status().is_success() {
            let status = response.status().as_u16();
            let message = response.text().await.unwrap_or_default();
            return Err(Error::from_status(status, message));
        }

        Ok(response)
    }
}

#[async_trait]
impl IssueProvider for GitLabClient {
    async fn get_issues(&self, filter: IssueFilter) -> Result<Vec<Issue>> {
        let mut url = self.api_url(&format!("/projects/{}/issues", self.project_id));
        let mut params = vec![];

        if let Some(state) = &filter.state {
            params.push(format!("state={}", state));
        }
        if let Some(search) = &filter.search {
            params.push(format!("search={}", search));
        }
        if let Some(limit) = filter.limit {
            params.push(format!("per_page={}", limit));
        }

        if !params.is_empty() {
            url.push_str(&format!("?{}", params.join("&")));
        }

        let response = self.get(&url).await?;

        // TODO: Map GitLab response to unified Issue type
        // For now, return empty vec as placeholder
        let _body: serde_json::Value = response
            .json()
            .await
            .map_err(|e| Error::Http(e.to_string()))?;

        // Placeholder - actual implementation would map GitLab issues to unified Issue
        Ok(vec![])
    }

    async fn get_issue(&self, key: &str) -> Result<Issue> {
        // Parse key like "gitlab#123" to get issue iid
        let iid = key
            .strip_prefix("gitlab#")
            .and_then(|s| s.parse::<u64>().ok())
            .ok_or_else(|| Error::InvalidData(format!("Invalid issue key: {}", key)))?;

        let url = self.api_url(&format!("/projects/{}/issues/{}", self.project_id, iid));
        let _response = self.get(&url).await?;

        // TODO: Map GitLab response to unified Issue type
        Err(Error::ProviderUnsupported {
            provider: "gitlab".to_string(),
            operation: "get_issue mapping not yet implemented".to_string(),
        })
    }

    async fn create_issue(&self, _input: CreateIssueInput) -> Result<Issue> {
        // TODO: Implement issue creation
        Err(Error::ProviderUnsupported {
            provider: "gitlab".to_string(),
            operation: "create_issue".to_string(),
        })
    }

    async fn update_issue(&self, _key: &str, _input: UpdateIssueInput) -> Result<Issue> {
        // TODO: Implement issue update
        Err(Error::ProviderUnsupported {
            provider: "gitlab".to_string(),
            operation: "update_issue".to_string(),
        })
    }

    async fn get_comments(&self, _issue_key: &str) -> Result<Vec<Comment>> {
        // TODO: Implement get comments
        Err(Error::ProviderUnsupported {
            provider: "gitlab".to_string(),
            operation: "get_comments".to_string(),
        })
    }

    async fn add_comment(&self, _issue_key: &str, _body: &str) -> Result<Comment> {
        // TODO: Implement add comment
        Err(Error::ProviderUnsupported {
            provider: "gitlab".to_string(),
            operation: "add_comment".to_string(),
        })
    }

    fn provider_name(&self) -> &'static str {
        "gitlab"
    }
}

#[async_trait]
impl MergeRequestProvider for GitLabClient {
    async fn get_merge_requests(&self, filter: MrFilter) -> Result<Vec<MergeRequest>> {
        let mut url = self.api_url(&format!("/projects/{}/merge_requests", self.project_id));
        let mut params = vec![];

        if let Some(state) = &filter.state {
            params.push(format!("state={}", state));
        }
        if let Some(limit) = filter.limit {
            params.push(format!("per_page={}", limit));
        }

        if !params.is_empty() {
            url.push_str(&format!("?{}", params.join("&")));
        }

        let _response = self.get(&url).await?;

        // TODO: Map GitLab response to unified MergeRequest type
        Ok(vec![])
    }

    async fn get_merge_request(&self, key: &str) -> Result<MergeRequest> {
        // Parse key like "mr#123" to get MR iid
        let iid = key
            .strip_prefix("mr#")
            .and_then(|s| s.parse::<u64>().ok())
            .ok_or_else(|| Error::InvalidData(format!("Invalid MR key: {}", key)))?;

        let url = self.api_url(&format!(
            "/projects/{}/merge_requests/{}",
            self.project_id, iid
        ));
        let _response = self.get(&url).await?;

        // TODO: Map GitLab response to unified MergeRequest type
        Err(Error::ProviderUnsupported {
            provider: "gitlab".to_string(),
            operation: "get_merge_request mapping not yet implemented".to_string(),
        })
    }

    async fn get_discussions(&self, _mr_key: &str) -> Result<Vec<Discussion>> {
        // TODO: Implement get discussions
        Err(Error::ProviderUnsupported {
            provider: "gitlab".to_string(),
            operation: "get_discussions".to_string(),
        })
    }

    async fn get_diffs(&self, _mr_key: &str) -> Result<Vec<FileDiff>> {
        // TODO: Implement get diffs
        Err(Error::ProviderUnsupported {
            provider: "gitlab".to_string(),
            operation: "get_diffs".to_string(),
        })
    }

    async fn add_comment(&self, _mr_key: &str, _input: CreateCommentInput) -> Result<Comment> {
        // TODO: Implement add comment to MR
        Err(Error::ProviderUnsupported {
            provider: "gitlab".to_string(),
            operation: "add_mr_comment".to_string(),
        })
    }

    fn provider_name(&self) -> &'static str {
        "gitlab"
    }
}

#[async_trait]
impl Provider for GitLabClient {
    async fn get_current_user(&self) -> Result<User> {
        let url = self.api_url("/user");
        let _response = self.get(&url).await?;

        // TODO: Map GitLab user response to unified User type
        Err(Error::ProviderUnsupported {
            provider: "gitlab".to_string(),
            operation: "get_current_user mapping not yet implemented".to_string(),
        })
    }
}
