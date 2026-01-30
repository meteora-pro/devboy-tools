//! Integration tests for GitHub provider.
//!
//! These tests implement the Record & Replay pattern from ADR-003:
//! - With GITHUB_TOKEN: calls real API and updates fixtures
//! - Without GITHUB_TOKEN: uses saved fixtures
//!
//! # Running Tests
//!
//! ```bash
//! # Replay mode (no token needed, can run in parallel)
//! cargo test --test github_test
//!
//! # Record mode (updates fixtures, must run sequentially)
//! GITHUB_TOKEN=your_token GITHUB_OWNER=owner GITHUB_REPO=repo \
//!     cargo test --test github_test -- --test-threads=1
//! ```
//!
//! Note: Record mode requires `--test-threads=1` because some tests temporarily
//! modify environment variables to test Replay mode, which can cause race
//! conditions when running in parallel.

mod common;

use common::TestProvider;
use devboy_core::{IssueFilter, IssueProvider, MergeRequestProvider, MrFilter, Provider};

/// Test that we can detect the correct test mode.
#[tokio::test]
async fn test_mode_detection() {
    let provider = TestProvider::github();

    // Mode should be Replay unless GITHUB_TOKEN is set
    if std::env::var("GITHUB_TOKEN").is_ok() {
        assert!(provider.mode().is_record(), "Expected Record mode with token");
    } else {
        assert!(
            provider.mode().is_replay(),
            "Expected Replay mode without token"
        );
    }
}

/// Test getting issues from GitHub.
#[tokio::test]
async fn test_get_issues() {
    let provider = TestProvider::github();

    let issues = provider.get_issues(IssueFilter::default()).await.unwrap();

    assert!(!issues.is_empty(), "Should have at least one issue");

    // Verify issue structure
    let issue = &issues[0];
    assert!(issue.key.starts_with("gh#"), "Issue key should start with gh#");
    assert!(!issue.title.is_empty(), "Issue should have a title");
    assert_eq!(issue.source, "github", "Source should be github");
}

/// Test getting issues with state filter.
#[tokio::test]
async fn test_get_issues_with_filter() {
    let provider = TestProvider::github();

    let filter = IssueFilter {
        state: Some("open".to_string()),
        limit: Some(5),
        ..Default::default()
    };

    let issues = provider.get_issues(filter).await.unwrap();

    // All issues should be open
    for issue in &issues {
        assert_eq!(issue.state, "open", "Issue should be open");
    }
}

/// Test getting a single issue.
#[tokio::test]
async fn test_get_issue() {
    let provider = TestProvider::github();

    // First get all issues
    let issues = provider.get_issues(IssueFilter::default()).await.unwrap();
    assert!(!issues.is_empty());

    // Then get a specific issue
    let key = &issues[0].key;
    let issue = provider.get_issue(key).await.unwrap();

    assert_eq!(&issue.key, key);
}

/// Test getting pull requests.
#[tokio::test]
async fn test_get_pull_requests() {
    let provider = TestProvider::github();

    let prs = provider
        .get_merge_requests(MrFilter::default())
        .await
        .unwrap();

    assert!(!prs.is_empty(), "Should have at least one PR");

    // Verify PR structure
    let pr = &prs[0];
    assert!(pr.key.starts_with("pr#"), "PR key should start with pr#");
    assert!(!pr.title.is_empty(), "PR should have a title");
    assert_eq!(pr.source, "github", "Source should be github");
    assert!(!pr.source_branch.is_empty(), "PR should have source branch");
    assert!(!pr.target_branch.is_empty(), "PR should have target branch");
}

/// Test getting pull requests with state filter.
#[tokio::test]
async fn test_get_pull_requests_with_filter() {
    let provider = TestProvider::github();

    let filter = MrFilter {
        state: Some("open".to_string()),
        limit: Some(5),
        ..Default::default()
    };

    let prs = provider.get_merge_requests(filter).await.unwrap();

    // All PRs should be open
    for pr in &prs {
        assert_eq!(pr.state, "open", "PR should be open");
    }
}

/// Test getting current user.
#[tokio::test]
async fn test_get_current_user() {
    let provider = TestProvider::github();

    let user = provider.get_current_user().await.unwrap();

    assert!(!user.username.is_empty(), "User should have a username");
    assert!(!user.id.is_empty(), "User should have an id");
}

/// Test provider name.
#[tokio::test]
async fn test_provider_name() {
    let provider = TestProvider::github();

    assert_eq!(provider.name(), "github");
}

/// Test that issues have proper URL format.
#[tokio::test]
async fn test_issue_url_format() {
    let provider = TestProvider::github();

    let issues = provider.get_issues(IssueFilter::default()).await.unwrap();
    assert!(!issues.is_empty());

    for issue in &issues {
        if let Some(url) = &issue.url {
            assert!(
                url.starts_with("https://github.com/"),
                "Issue URL should be a GitHub URL: {}",
                url
            );
        }
    }
}

/// Test that PRs have proper URL format.
#[tokio::test]
async fn test_pr_url_format() {
    let provider = TestProvider::github();

    let prs = provider
        .get_merge_requests(MrFilter::default())
        .await
        .unwrap();
    assert!(!prs.is_empty());

    for pr in &prs {
        if let Some(url) = &pr.url {
            assert!(
                url.starts_with("https://github.com/"),
                "PR URL should be a GitHub URL: {}",
                url
            );
        }
    }
}
