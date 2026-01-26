//! GitHub API client implementation.

use async_trait::async_trait;
use devboy_core::types::{Issue, MergeRequest, User};
use devboy_core::{Error, Provider, Result};

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
}

#[async_trait]
impl Provider for GitHubClient {
    fn name(&self) -> &str {
        "github"
    }

    async fn get_issues(&self, state: Option<&str>) -> Result<Vec<Issue>> {
        let mut url = format!("{}/repos/{}/{}/issues", self.base_url, self.owner, self.repo);
        if let Some(s) = state {
            url.push_str(&format!("?state={}", s));
        }

        let response = self
            .client
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.token))
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .send()
            .await
            .map_err(|e| Error::Http(e.to_string()))?;

        if !response.status().is_success() {
            return Err(Error::Api {
                status: response.status().as_u16(),
                message: response.text().await.unwrap_or_default(),
            });
        }

        // Note: GitHub issues response needs mapping to our Issue type
        // This is a placeholder - actual implementation would need proper mapping
        response
            .json()
            .await
            .map_err(|e| Error::Http(e.to_string()))
    }

    async fn get_issue(&self, id: u64) -> Result<Issue> {
        let url = format!(
            "{}/repos/{}/{}/issues/{}",
            self.base_url, self.owner, self.repo, id
        );

        let response = self
            .client
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.token))
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .send()
            .await
            .map_err(|e| Error::Http(e.to_string()))?;

        if !response.status().is_success() {
            return Err(Error::Api {
                status: response.status().as_u16(),
                message: response.text().await.unwrap_or_default(),
            });
        }

        response
            .json()
            .await
            .map_err(|e| Error::Http(e.to_string()))
    }

    async fn get_merge_requests(&self, state: Option<&str>) -> Result<Vec<MergeRequest>> {
        let mut url = format!("{}/repos/{}/{}/pulls", self.base_url, self.owner, self.repo);
        if let Some(s) = state {
            url.push_str(&format!("?state={}", s));
        }

        let response = self
            .client
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.token))
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .send()
            .await
            .map_err(|e| Error::Http(e.to_string()))?;

        if !response.status().is_success() {
            return Err(Error::Api {
                status: response.status().as_u16(),
                message: response.text().await.unwrap_or_default(),
            });
        }

        response
            .json()
            .await
            .map_err(|e| Error::Http(e.to_string()))
    }

    async fn get_merge_request(&self, id: u64) -> Result<MergeRequest> {
        let url = format!(
            "{}/repos/{}/{}/pulls/{}",
            self.base_url, self.owner, self.repo, id
        );

        let response = self
            .client
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.token))
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .send()
            .await
            .map_err(|e| Error::Http(e.to_string()))?;

        if !response.status().is_success() {
            return Err(Error::Api {
                status: response.status().as_u16(),
                message: response.text().await.unwrap_or_default(),
            });
        }

        response
            .json()
            .await
            .map_err(|e| Error::Http(e.to_string()))
    }

    async fn get_current_user(&self) -> Result<User> {
        let url = format!("{}/user", self.base_url);

        let response = self
            .client
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.token))
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .send()
            .await
            .map_err(|e| Error::Http(e.to_string()))?;

        if !response.status().is_success() {
            return Err(Error::Api {
                status: response.status().as_u16(),
                message: response.text().await.unwrap_or_default(),
            });
        }

        response
            .json()
            .await
            .map_err(|e| Error::Http(e.to_string()))
    }
}
