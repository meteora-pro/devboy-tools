//! Test provider with Record/Replay support.
//!
//! Implements the Record & Replay pattern from ADR-003.

use std::env;

use async_trait::async_trait;
use devboy_core::{
    Comment, CreateCommentInput, CreateIssueInput, Discussion, Error, FileDiff, Issue, IssueFilter,
    IssueProvider, MergeRequest, MergeRequestProvider, MrFilter, Provider, ProviderResult, Result,
    UpdateIssueInput, User,
};
use devboy_github::GitHubClient;
use secrecy::SecretString;

use super::api_result::ApiResult;
use super::{FixtureProvider, TestMode};

/// Test provider that supports Record/Replay modes.
///
/// In Record mode: calls real API and saves responses to fixtures.
/// In Replay mode: loads data from fixtures.
pub struct TestProvider {
    mode: TestMode,
    provider_name: String,
    github_client: Option<GitHubClient>,
    fixture_provider: FixtureProvider,
}

impl TestProvider {
    /// Create a new test provider for GitHub.
    ///
    /// Detects mode based on GITHUB_TOKEN environment variable.
    pub fn github() -> Self {
        Self::new("github")
    }

    /// Create a new test provider.
    fn new(provider_name: &str) -> Self {
        let mode = TestMode::detect(provider_name);

        let github_client = if mode.is_record() && provider_name == "github" {
            // Get GitHub configuration from environment
            let token = env::var("GITHUB_TOKEN").ok();
            let owner = env::var("GITHUB_OWNER").unwrap_or_else(|_| "meteora-pro".to_string());
            let repo = env::var("GITHUB_REPO").unwrap_or_else(|_| "devboy-tools".to_string());

            token.map(|t| GitHubClient::new(&owner, &repo, SecretString::from(t)))
        } else {
            None
        };

        Self {
            mode,
            provider_name: provider_name.to_string(),
            github_client,
            fixture_provider: FixtureProvider::new(provider_name),
        }
    }

    /// Get the test mode.
    pub fn mode(&self) -> TestMode {
        self.mode
    }

    /// Get the provider name.
    pub fn name(&self) -> &str {
        &self.provider_name
    }

    /// Get issues with fallback support.
    pub async fn get_issues_with_fallback(&self, filter: IssueFilter) -> ApiResult<Vec<Issue>> {
        match self.mode {
            TestMode::Replay => {
                // Load from fixtures
                match self.fixture_provider.load_issues() {
                    Ok(issues) => ApiResult::Ok(issues),
                    Err(e) => ApiResult::ConfigError {
                        message: format!("Failed to load fixtures: {}", e),
                    },
                }
            }
            TestMode::Record => {
                let Some(client) = &self.github_client else {
                    return ApiResult::ConfigError {
                        message: "GitHub client not initialized".to_string(),
                    };
                };

                match client.get_issues(filter).await {
                    Ok(result) => {
                        let issues = result.items;
                        // Save to fixtures for future replay
                        if let Err(e) = self.fixture_provider.save_issues(&issues) {
                            eprintln!("⚠️  Failed to save fixtures: {}", e);
                        }
                        ApiResult::Ok(issues)
                    }
                    Err(e) => self.handle_api_error(e, || self.fixture_provider.load_issues()),
                }
            }
        }
    }

    /// Get merge requests with fallback support.
    pub async fn get_merge_requests_with_fallback(
        &self,
        filter: MrFilter,
    ) -> ApiResult<Vec<MergeRequest>> {
        match self.mode {
            TestMode::Replay => {
                // Load from fixtures
                match self.fixture_provider.load_merge_requests() {
                    Ok(mrs) => ApiResult::Ok(mrs),
                    Err(e) => ApiResult::ConfigError {
                        message: format!("Failed to load fixtures: {}", e),
                    },
                }
            }
            TestMode::Record => {
                let Some(client) = &self.github_client else {
                    return ApiResult::ConfigError {
                        message: "GitHub client not initialized".to_string(),
                    };
                };

                match client.get_merge_requests(filter).await {
                    Ok(result) => {
                        let mrs = result.items;
                        // Save to fixtures for future replay
                        if let Err(e) = self.fixture_provider.save_merge_requests(&mrs) {
                            eprintln!("⚠️  Failed to save fixtures: {}", e);
                        }
                        ApiResult::Ok(mrs)
                    }
                    Err(e) => {
                        self.handle_api_error(e, || self.fixture_provider.load_merge_requests())
                    }
                }
            }
        }
    }

    /// Get current user with fallback support.
    pub async fn get_current_user_with_fallback(&self) -> ApiResult<User> {
        match self.mode {
            TestMode::Replay => {
                // Return a mock user for replay mode
                ApiResult::Ok(User {
                    id: "1".to_string(),
                    username: "test-user".to_string(),
                    name: Some("Test User".to_string()),
                    email: None,
                    avatar_url: None,
                })
            }
            TestMode::Record => {
                let Some(client) = &self.github_client else {
                    return ApiResult::ConfigError {
                        message: "GitHub client not initialized".to_string(),
                    };
                };

                match client.get_current_user().await {
                    Ok(user) => ApiResult::Ok(user),
                    Err(e) => {
                        if e.is_auth_error() {
                            ApiResult::ConfigError {
                                message: format!("Authentication error: {}", e),
                            }
                        } else {
                            eprintln!("⚠️  API error, using mock user: {}", e);
                            ApiResult::Fallback {
                                data: User {
                                    id: "1".to_string(),
                                    username: "test-user".to_string(),
                                    name: Some("Test User".to_string()),
                                    email: None,
                                    avatar_url: None,
                                },
                                reason: format!("API error: {}", e),
                            }
                        }
                    }
                }
            }
        }
    }

    /// Handle API errors with fallback logic.
    ///
    /// - 401/403: Configuration error (test fails)
    /// - 5xx/Network: Fallback to fixtures
    fn handle_api_error<T, F>(&self, error: Error, load_fixture: F) -> ApiResult<T>
    where
        F: FnOnce() -> Result<T>,
    {
        // Authentication errors - test should fail
        if error.is_auth_error() {
            return ApiResult::ConfigError {
                message: format!("Authentication error: {}", error),
            };
        }

        // Check if retryable (5xx, network errors, etc.)
        if error.is_retryable() {
            eprintln!("⚠️  Retryable error, falling back to fixtures: {}", error);
            match load_fixture() {
                Ok(data) => ApiResult::Fallback {
                    data,
                    reason: format!("Retryable error: {}", error),
                },
                Err(e) => ApiResult::ConfigError {
                    message: format!("API failed and fixtures unavailable: {}", e),
                },
            }
        } else {
            // Non-retryable API errors - also try fallback
            eprintln!("⚠️  API error, falling back to fixtures: {}", error);
            match load_fixture() {
                Ok(data) => ApiResult::Fallback {
                    data,
                    reason: format!("API error: {}", error),
                },
                Err(e) => ApiResult::ConfigError {
                    message: format!("API failed and fixtures unavailable: {}", e),
                },
            }
        }
    }
}

/// Implement IssueProvider for TestProvider.
#[async_trait]
impl IssueProvider for TestProvider {
    async fn get_issues(&self, filter: IssueFilter) -> Result<ProviderResult<Issue>> {
        self.get_issues_with_fallback(filter)
            .await
            .into_result()
            .map(|v| v.into())
            .map_err(Error::Config)
    }

    async fn get_issue(&self, key: &str) -> Result<Issue> {
        // Find in the list
        let result = self.get_issues(IssueFilter::default()).await?;
        result
            .items
            .into_iter()
            .find(|i| i.key == key)
            .ok_or_else(|| Error::NotFound(format!("Issue {} not found", key)))
    }

    async fn create_issue(&self, _input: CreateIssueInput) -> Result<Issue> {
        Err(Error::Config(
            "Create issue not supported in tests".to_string(),
        ))
    }

    async fn update_issue(&self, _key: &str, _input: UpdateIssueInput) -> Result<Issue> {
        Err(Error::Config(
            "Update issue not supported in tests".to_string(),
        ))
    }

    async fn get_comments(&self, issue_key: &str) -> Result<ProviderResult<Comment>> {
        if self.mode.is_record() {
            let Some(client) = &self.github_client else {
                return Err(Error::Config("GitHub client not initialized".to_string()));
            };
            client.get_comments(issue_key).await
        } else {
            // In replay mode, return mock comments
            Ok(vec![Comment {
                id: "1".to_string(),
                body: "Test comment".to_string(),
                author: None,
                created_at: Some("2024-01-01T00:00:00Z".to_string()),
                updated_at: None,
                position: None,
            }]
            .into())
        }
    }

    async fn add_comment(&self, _issue_key: &str, _body: &str) -> Result<Comment> {
        Err(Error::Config(
            "Add comment not supported in tests".to_string(),
        ))
    }

    fn provider_name(&self) -> &'static str {
        "github"
    }
}

/// Implement MergeRequestProvider for TestProvider.
#[async_trait]
impl MergeRequestProvider for TestProvider {
    async fn get_merge_requests(&self, filter: MrFilter) -> Result<ProviderResult<MergeRequest>> {
        self.get_merge_requests_with_fallback(filter)
            .await
            .into_result()
            .map(|v| v.into())
            .map_err(Error::Config)
    }

    async fn get_merge_request(&self, key: &str) -> Result<MergeRequest> {
        let result = self.get_merge_requests(MrFilter::default()).await?;
        result
            .items
            .into_iter()
            .find(|mr| mr.key == key)
            .ok_or_else(|| Error::NotFound(format!("MR {} not found", key)))
    }

    async fn get_discussions(&self, mr_key: &str) -> Result<ProviderResult<Discussion>> {
        if self.mode.is_record() {
            let Some(client) = &self.github_client else {
                return Err(Error::Config("GitHub client not initialized".to_string()));
            };
            client.get_discussions(mr_key).await
        } else {
            // In replay mode, return mock discussions
            Ok(vec![Discussion {
                id: "1".to_string(),
                resolved: false,
                resolved_by: None,
                comments: vec![Comment {
                    id: "1".to_string(),
                    body: "Review comment".to_string(),
                    author: None,
                    created_at: Some("2024-01-01T00:00:00Z".to_string()),
                    updated_at: None,
                    position: None,
                }],
                position: None,
            }]
            .into())
        }
    }

    async fn get_diffs(&self, mr_key: &str) -> Result<ProviderResult<FileDiff>> {
        if self.mode.is_record() {
            let Some(client) = &self.github_client else {
                return Err(Error::Config("GitHub client not initialized".to_string()));
            };
            client.get_diffs(mr_key).await
        } else {
            // In replay mode, return mock diffs
            Ok(vec![FileDiff {
                file_path: "src/main.rs".to_string(),
                old_path: None,
                new_file: false,
                deleted_file: false,
                renamed_file: false,
                diff: "+added line\n-removed line".to_string(),
                additions: Some(1),
                deletions: Some(1),
            }]
            .into())
        }
    }

    async fn add_comment(&self, _mr_key: &str, _input: CreateCommentInput) -> Result<Comment> {
        Err(Error::Config(
            "Add comment not supported in tests".to_string(),
        ))
    }

    fn provider_name(&self) -> &'static str {
        "github"
    }
}

#[async_trait]
impl devboy_core::PipelineProvider for TestProvider {
    fn provider_name(&self) -> &'static str {
        "test"
    }
}

/// Implement Provider for TestProvider.
#[async_trait]
impl Provider for TestProvider {
    async fn get_current_user(&self) -> Result<User> {
        self.get_current_user_with_fallback()
            .await
            .into_result()
            .map_err(Error::Config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_provider_replay_mode() {
        // Temporarily remove token to test replay mode
        temp_env::with_var_unset("GITHUB_TOKEN", || {
            let provider = TestProvider::github();
            assert!(provider.mode().is_replay());
        });
    }

    #[tokio::test]
    async fn test_provider_loads_fixtures_in_replay() {
        temp_env::async_with_vars([("GITHUB_TOKEN", None::<&str>)], async {
            let provider = TestProvider::github();
            let issues = provider
                .get_issues(IssueFilter::default())
                .await
                .unwrap()
                .items;
            assert!(!issues.is_empty());
            assert!(issues[0].key.starts_with("gh#"));
        })
        .await;
    }

    #[tokio::test]
    async fn test_provider_loads_mrs_in_replay() {
        temp_env::async_with_vars([("GITHUB_TOKEN", None::<&str>)], async {
            let provider = TestProvider::github();
            let mrs = provider
                .get_merge_requests(MrFilter::default())
                .await
                .unwrap()
                .items;
            assert!(!mrs.is_empty());
            assert!(mrs[0].key.starts_with("pr#"));
        })
        .await;
    }
}
