//! Linear GraphQL client implementation.

use async_trait::async_trait;
use devboy_core::{
    Comment, CreateIssueInput, Error, Issue, IssueFilter, IssueProvider, MergeRequestProvider,
    PipelineProvider, Provider, ProviderResult, Result, UpdateIssueInput, User,
};
use secrecy::{ExposeSecret, SecretString};
use serde_json::{Value, json};
use tracing::debug;

use crate::DEFAULT_LINEAR_URL;
use crate::types::{GraphQlResponse, Viewer, ViewerData};

const VIEWER_QUERY: &str = r#"
query Viewer {
  viewer {
    id
    name
    displayName
    email
  }
}
"#;

pub struct LinearClient {
    base_url: String,
    team_id: String,
    team_key: Option<String>,
    token: SecretString,
    http: reqwest::Client,
}

impl LinearClient {
    pub fn new(team_id: impl Into<String>, token: SecretString) -> Self {
        Self::with_base_url(DEFAULT_LINEAR_URL, team_id, token)
    }

    pub fn with_base_url(
        base_url: impl Into<String>,
        team_id: impl Into<String>,
        token: SecretString,
    ) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            team_id: team_id.into(),
            team_key: None,
            token,
            http: reqwest::Client::builder()
                .user_agent("devboy-tools")
                .build()
                .expect("Failed to create HTTP client"),
        }
    }

    pub fn with_team_key(mut self, team_key: impl Into<String>) -> Self {
        self.team_key = Some(team_key.into());
        self
    }

    pub fn team_id(&self) -> &str {
        &self.team_id
    }

    pub fn team_key(&self) -> Option<&str> {
        self.team_key.as_deref()
    }

    pub(crate) async fn viewer_with_token(&self, token: &SecretString) -> Result<Viewer> {
        let data: ViewerData = self.graphql(VIEWER_QUERY, json!({}), token).await?;
        Ok(data.viewer)
    }

    async fn graphql<T: serde::de::DeserializeOwned>(
        &self,
        query: &str,
        variables: Value,
        token: &SecretString,
    ) -> Result<T> {
        let body = json!({
            "query": query,
            "variables": variables,
        });

        debug!(url = %self.base_url, "linear graphql request");

        let response = self
            .http
            .post(&self.base_url)
            .header("Authorization", token.expose_secret())
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| Error::Network(e.to_string()))?;

        let status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(Error::Unauthorized("Invalid Linear API token".to_string()));
        }
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err(Error::RateLimited { retry_after: None });
        }
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(Error::Api {
                status: status.as_u16(),
                message: text,
            });
        }

        let gql_response: GraphQlResponse<T> = response
            .json()
            .await
            .map_err(|e| Error::InvalidData(e.to_string()))?;

        if !gql_response.errors.is_empty() {
            let rate_limited = gql_response.errors.iter().any(|e| {
                e.extensions.as_ref().and_then(|x| x.code.as_deref()) == Some("RATELIMITED")
            });
            let message = gql_response
                .errors
                .into_iter()
                .map(|e| e.message)
                .collect::<Vec<_>>()
                .join("; ");
            return if rate_limited {
                let _ = message;
                Err(Error::RateLimited { retry_after: None })
            } else {
                Err(Error::Api {
                    status: 200,
                    message,
                })
            };
        }

        gql_response
            .data
            .ok_or_else(|| Error::InvalidData("Linear API returned no data".to_string()))
    }

    fn unsupported<T>(&self, operation: &str) -> Result<T> {
        Err(Error::ProviderUnsupported {
            provider: "linear".to_string(),
            operation: operation.to_string(),
        })
    }
}

#[async_trait]
impl IssueProvider for LinearClient {
    async fn get_issues(&self, _filter: IssueFilter) -> Result<ProviderResult<Issue>> {
        self.unsupported("get_issues")
    }

    async fn get_issue(&self, _key: &str) -> Result<Issue> {
        self.unsupported("get_issue")
    }

    async fn create_issue(&self, _input: CreateIssueInput) -> Result<Issue> {
        self.unsupported("create_issue")
    }

    async fn update_issue(&self, _key: &str, _input: UpdateIssueInput) -> Result<Issue> {
        self.unsupported("update_issue")
    }

    async fn get_comments(&self, _issue_key: &str) -> Result<ProviderResult<Comment>> {
        self.unsupported("get_comments")
    }

    async fn add_comment(&self, _issue_key: &str, _body: &str) -> Result<Comment> {
        self.unsupported("add_comment")
    }

    fn provider_name(&self) -> &'static str {
        "linear"
    }
}

#[async_trait]
impl MergeRequestProvider for LinearClient {
    fn provider_name(&self) -> &'static str {
        "linear"
    }
}

#[async_trait]
impl PipelineProvider for LinearClient {
    fn provider_name(&self) -> &'static str {
        "linear"
    }
}

#[async_trait]
impl Provider for LinearClient {
    async fn get_current_user(&self) -> Result<User> {
        let viewer = self.viewer_with_token(&self.token).await?;
        Ok(User {
            id: viewer.id,
            username: viewer
                .display_name
                .clone()
                .unwrap_or_else(|| viewer.name.clone()),
            name: Some(viewer.name),
            email: viewer.email,
            avatar_url: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use httpmock::Method::POST;
    use httpmock::MockServer;

    #[tokio::test]
    async fn get_current_user_reads_viewer() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST).path("/graphql");
            then.status(200)
                .header("content-type", "application/json")
                .body(r#"{"data":{"viewer":{"id":"u1","name":"Alice","displayName":"alice","email":"alice@example.com"}}}"#);
        });

        let client = LinearClient::with_base_url(
            format!("{}/graphql", server.base_url()),
            "team-1",
            SecretString::from("lin_api_test".to_owned()),
        );

        let user = client.get_current_user().await.unwrap();
        assert_eq!(user.id, "u1");
        assert_eq!(user.username, "alice");
        assert_eq!(user.name.as_deref(), Some("Alice"));
        assert_eq!(user.email.as_deref(), Some("alice@example.com"));
        mock.assert();
    }
}
