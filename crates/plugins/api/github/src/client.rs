//! GitHub API client implementation.

use async_trait::async_trait;
use devboy_core::{
    Comment, CreateCommentInput, CreateIssueInput, Discussion, Error, FileDiff, Issue, IssueFilter,
    IssueProvider, MergeRequest, MergeRequestProvider, MrFilter, Provider, Result,
    UpdateIssueInput, User,
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

    /// Make an authenticated GET request.
    async fn get(&self, url: &str) -> Result<reqwest::Response> {
        let response = self
            .client
            .get(url)
            .header("Authorization", format!("Bearer {}", self.token))
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
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
impl IssueProvider for GitHubClient {
    async fn get_issues(&self, filter: IssueFilter) -> Result<Vec<Issue>> {
        let mut url = format!(
            "{}/repos/{}/{}/issues",
            self.base_url, self.owner, self.repo
        );
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

        // TODO: Map GitHub response to unified Issue type
        Ok(vec![])
    }

    async fn get_issue(&self, key: &str) -> Result<Issue> {
        // Parse key like "gh#123" to get issue number
        let number = key
            .strip_prefix("gh#")
            .and_then(|s| s.parse::<u64>().ok())
            .ok_or_else(|| Error::InvalidData(format!("Invalid issue key: {}", key)))?;

        let url = format!(
            "{}/repos/{}/{}/issues/{}",
            self.base_url, self.owner, self.repo, number
        );
        let _response = self.get(&url).await?;

        // TODO: Map GitHub response to unified Issue type
        Err(Error::ProviderUnsupported {
            provider: "github".to_string(),
            operation: "get_issue mapping not yet implemented".to_string(),
        })
    }

    async fn create_issue(&self, _input: CreateIssueInput) -> Result<Issue> {
        // TODO: Implement issue creation
        Err(Error::ProviderUnsupported {
            provider: "github".to_string(),
            operation: "create_issue".to_string(),
        })
    }

    async fn update_issue(&self, _key: &str, _input: UpdateIssueInput) -> Result<Issue> {
        // TODO: Implement issue update
        Err(Error::ProviderUnsupported {
            provider: "github".to_string(),
            operation: "update_issue".to_string(),
        })
    }

    async fn get_comments(&self, _issue_key: &str) -> Result<Vec<Comment>> {
        // TODO: Implement get comments
        Err(Error::ProviderUnsupported {
            provider: "github".to_string(),
            operation: "get_comments".to_string(),
        })
    }

    async fn add_comment(&self, _issue_key: &str, _body: &str) -> Result<Comment> {
        // TODO: Implement add comment
        Err(Error::ProviderUnsupported {
            provider: "github".to_string(),
            operation: "add_comment".to_string(),
        })
    }

    fn provider_name(&self) -> &'static str {
        "github"
    }
}

#[async_trait]
impl MergeRequestProvider for GitHubClient {
    async fn get_merge_requests(&self, filter: MrFilter) -> Result<Vec<MergeRequest>> {
        let mut url = format!("{}/repos/{}/{}/pulls", self.base_url, self.owner, self.repo);
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

        // TODO: Map GitHub response to unified MergeRequest type
        Ok(vec![])
    }

    async fn get_merge_request(&self, key: &str) -> Result<MergeRequest> {
        // Parse key like "pr#123" to get PR number
        let number = key
            .strip_prefix("pr#")
            .and_then(|s| s.parse::<u64>().ok())
            .ok_or_else(|| Error::InvalidData(format!("Invalid PR key: {}", key)))?;

        let url = format!(
            "{}/repos/{}/{}/pulls/{}",
            self.base_url, self.owner, self.repo, number
        );
        let _response = self.get(&url).await?;

        // TODO: Map GitHub response to unified MergeRequest type
        Err(Error::ProviderUnsupported {
            provider: "github".to_string(),
            operation: "get_merge_request mapping not yet implemented".to_string(),
        })
    }

    async fn get_discussions(&self, _mr_key: &str) -> Result<Vec<Discussion>> {
        // TODO: Implement get discussions (PR reviews)
        Err(Error::ProviderUnsupported {
            provider: "github".to_string(),
            operation: "get_discussions".to_string(),
        })
    }

    async fn get_diffs(&self, _mr_key: &str) -> Result<Vec<FileDiff>> {
        // TODO: Implement get diffs
        Err(Error::ProviderUnsupported {
            provider: "github".to_string(),
            operation: "get_diffs".to_string(),
        })
    }

    async fn add_comment(&self, _mr_key: &str, _input: CreateCommentInput) -> Result<Comment> {
        // TODO: Implement add comment to PR
        Err(Error::ProviderUnsupported {
            provider: "github".to_string(),
            operation: "add_pr_comment".to_string(),
        })
    }

    fn provider_name(&self) -> &'static str {
        "github"
    }
}

#[async_trait]
impl Provider for GitHubClient {
    async fn get_current_user(&self) -> Result<User> {
        let url = format!("{}/user", self.base_url);
        let _response = self.get(&url).await?;

        // TODO: Map GitHub user response to unified User type
        Err(Error::ProviderUnsupported {
            provider: "github".to_string(),
            operation: "get_current_user mapping not yet implemented".to_string(),
        })
    }
}
