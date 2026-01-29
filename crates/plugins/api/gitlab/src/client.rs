//! GitLab API client implementation.

use async_trait::async_trait;
use devboy_core::types::{Issue, MergeRequest, User};
use devboy_core::{Error, Provider, Result};

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
}

#[async_trait]
impl Provider for GitLabClient {
    fn name(&self) -> &str {
        "gitlab"
    }

    async fn get_issues(&self, state: Option<&str>) -> Result<Vec<Issue>> {
        let mut url = self.api_url(&format!("/projects/{}/issues", self.project_id));
        if let Some(s) = state {
            url.push_str(&format!("?state={}", s));
        }

        let response = self
            .client
            .get(&url)
            .header("PRIVATE-TOKEN", &self.token)
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

    async fn get_issue(&self, id: u64) -> Result<Issue> {
        let url = self.api_url(&format!("/projects/{}/issues/{}", self.project_id, id));

        let response = self
            .client
            .get(&url)
            .header("PRIVATE-TOKEN", &self.token)
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
        let mut url = self.api_url(&format!("/projects/{}/merge_requests", self.project_id));
        if let Some(s) = state {
            url.push_str(&format!("?state={}", s));
        }

        let response = self
            .client
            .get(&url)
            .header("PRIVATE-TOKEN", &self.token)
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
        let url = self.api_url(&format!(
            "/projects/{}/merge_requests/{}",
            self.project_id, id
        ));

        let response = self
            .client
            .get(&url)
            .header("PRIVATE-TOKEN", &self.token)
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
        let url = self.api_url("/user");

        let response = self
            .client
            .get(&url)
            .header("PRIVATE-TOKEN", &self.token)
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
