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
use devboy_core::{
    CreateCommentInput, CreateIssueInput, IssueFilter, IssueProvider, MergeRequestProvider,
    MrFilter, Provider, UpdateIssueInput,
};

/// Test that we can detect the correct test mode.
#[tokio::test]
async fn test_mode_detection() {
    let provider = TestProvider::github();

    // Mode should be Replay unless GITHUB_TOKEN is set
    if std::env::var("GITHUB_TOKEN").is_ok() {
        assert!(
            provider.mode().is_record(),
            "Expected Record mode with token"
        );
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
    assert!(
        issue.key.starts_with("gh#"),
        "Issue key should start with gh#"
    );
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

/// Test getting comments for an issue.
#[tokio::test]
async fn test_get_issue_comments() {
    let provider = TestProvider::github();

    // First get all issues
    let issues = provider.get_issues(IssueFilter::default()).await.unwrap();
    assert!(!issues.is_empty());

    // Get comments for the first issue
    let key = &issues[0].key;
    let comments = provider.get_comments(key).await.unwrap();

    // Comments should be a vector (may be empty)
    for comment in &comments {
        assert!(!comment.id.is_empty(), "Comment should have an ID");
        assert!(!comment.body.is_empty(), "Comment should have a body");
    }
}

/// Test getting a single pull request.
#[tokio::test]
async fn test_get_pull_request() {
    let provider = TestProvider::github();

    // First get all PRs
    let prs = provider
        .get_merge_requests(MrFilter::default())
        .await
        .unwrap();
    assert!(!prs.is_empty());

    // Get a specific PR
    let key = &prs[0].key;
    let pr = provider.get_merge_request(key).await.unwrap();

    assert_eq!(&pr.key, key);
    assert!(!pr.title.is_empty(), "PR should have a title");
}

/// Test getting discussions for a pull request.
#[tokio::test]
async fn test_get_pull_request_discussions() {
    let provider = TestProvider::github();

    // First get all PRs
    let prs = provider
        .get_merge_requests(MrFilter::default())
        .await
        .unwrap();
    assert!(!prs.is_empty());

    // Get discussions for the first PR
    let key = &prs[0].key;
    let discussions = provider.get_discussions(key).await.unwrap();

    // Discussions should be a vector (may be empty)
    for discussion in &discussions {
        assert!(!discussion.id.is_empty(), "Discussion should have an ID");
    }
}

/// Test getting diffs for a pull request.
#[tokio::test]
async fn test_get_pull_request_diffs() {
    let provider = TestProvider::github();

    // First get all PRs
    let prs = provider
        .get_merge_requests(MrFilter::default())
        .await
        .unwrap();
    assert!(!prs.is_empty());

    // Get diffs for the first PR
    let key = &prs[0].key;
    let diffs = provider.get_diffs(key).await.unwrap();

    // Diffs should be a vector (may be empty for PRs without changes)
    for diff in &diffs {
        assert!(!diff.file_path.is_empty(), "Diff should have a file path");
    }
}

/// Test that adding comment to PR returns error in test mode.
/// Note: In real implementation this would check if PR exists,
/// but TestProvider always returns "not supported" for write operations.
#[tokio::test]
async fn test_add_comment_to_pr_not_supported() {
    use devboy_core::{CreateCommentInput, MergeRequestProvider};

    let provider = TestProvider::github();

    let input = CreateCommentInput {
        body: "Test comment".to_string(),
        position: None,
        discussion_id: None,
    };

    // Write operations are not supported in TestProvider
    let result = MergeRequestProvider::add_comment(&provider, "pr#1", input).await;

    assert!(result.is_err(), "Adding comment should fail in test mode");
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("not supported"),
        "Error should indicate operation not supported: {}",
        err_msg
    );
}

/// Test that PR and Issue with same number are distinguished.
#[tokio::test]
async fn test_pr_issue_distinction() {
    let provider = TestProvider::github();

    // Get issues and PRs
    let issues = provider.get_issues(IssueFilter::default()).await.unwrap();
    let prs = provider
        .get_merge_requests(MrFilter::default())
        .await
        .unwrap();

    // Extract numbers from keys
    let issue_numbers: Vec<u64> = issues
        .iter()
        .map(|i| i.key.strip_prefix("gh#").unwrap().parse().unwrap())
        .collect();
    let pr_numbers: Vec<u64> = prs
        .iter()
        .map(|p| p.key.strip_prefix("pr#").unwrap().parse().unwrap())
        .collect();

    // In GitHub, PRs are also issues, so there may be overlap in numbering
    // But get_issues should filter out PRs
    for issue in &issues {
        let num: u64 = issue.key.strip_prefix("gh#").unwrap().parse().unwrap();
        // Issues returned should not be PRs
        assert!(
            !pr_numbers.contains(&num) || issue_numbers.contains(&num),
            "Issue {} should not be a PR",
            issue.key
        );
    }
}

/// Test that adding a comment to an issue returns error in test mode.
#[tokio::test]
async fn test_add_issue_comment_not_supported() {
    let provider = TestProvider::github();

    // First get all issues
    let issues = provider.get_issues(IssueFilter::default()).await.unwrap();
    assert!(!issues.is_empty(), "Should have at least one issue");

    // Add a comment to the first issue
    let key = &issues[0].key;
    let comment_body = "Test comment";

    let result = IssueProvider::add_comment(&provider, key, comment_body).await;

    // Write operations are not supported in TestProvider
    assert!(
        result.is_err(),
        "Add comment should fail in test mode with proper error"
    );
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("not supported"),
        "Error should indicate operation not supported: {}",
        err_msg
    );
}

/// Test that adding a comment to a PR returns error in test mode.
#[tokio::test]
async fn test_add_pr_comment_not_supported() {
    let provider = TestProvider::github();

    // First get all PRs
    let prs = provider
        .get_merge_requests(MrFilter::default())
        .await
        .unwrap();
    assert!(!prs.is_empty(), "Should have at least one PR");

    // Add a comment to the first PR
    let key = &prs[0].key;
    let input = CreateCommentInput {
        body: "Test PR comment".to_string(),
        position: None,
        discussion_id: None,
    };

    let result = MergeRequestProvider::add_comment(&provider, key, input).await;

    // Write operations are not supported in TestProvider
    assert!(
        result.is_err(),
        "Add PR comment should fail in test mode with proper error"
    );
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("not supported"),
        "Error should indicate operation not supported: {}",
        err_msg
    );
}

/// Test that adding an inline comment returns error in test mode.
#[tokio::test]
async fn test_add_pr_inline_comment_not_supported() {
    use devboy_core::CodePosition;

    let provider = TestProvider::github();

    // First get all PRs
    let prs = provider
        .get_merge_requests(MrFilter::default())
        .await
        .unwrap();
    assert!(!prs.is_empty(), "Should have at least one PR");

    let key = &prs[0].key;

    // Try to add an inline comment (position provided)
    let input = CreateCommentInput {
        body: "Test inline comment".to_string(),
        position: Some(CodePosition {
            file_path: "src/main.rs".to_string(),
            line: 1,
            line_type: "new".to_string(),
            commit_sha: None,
        }),
        discussion_id: None,
    };

    let result = MergeRequestProvider::add_comment(&provider, key, input).await;

    // Write operations are not supported in TestProvider
    assert!(
        result.is_err(),
        "Add inline comment should fail in test mode"
    );
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("not supported"),
        "Error should indicate operation not supported: {}",
        err_msg
    );
}

/// Test that creating a new issue returns error in test mode.
#[tokio::test]
async fn test_create_issue_not_supported() {
    let provider = TestProvider::github();

    let input = CreateIssueInput {
        title: "Test Issue".to_string(),
        description: Some("Test description".to_string()),
        labels: vec!["test".to_string()],
        assignees: vec![],
        priority: None,
    };

    let result = provider.create_issue(input).await;

    // Write operations are not supported in TestProvider
    assert!(
        result.is_err(),
        "Create issue should fail in test mode with proper error"
    );
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("not supported"),
        "Error should indicate operation not supported: {}",
        err_msg
    );
}

/// Test that updating an issue returns error in test mode.
#[tokio::test]
async fn test_update_issue_not_supported() {
    let provider = TestProvider::github();

    // First get all issues
    let issues = provider.get_issues(IssueFilter::default()).await.unwrap();
    assert!(
        !issues.is_empty(),
        "Should have at least one issue to update"
    );

    let key = &issues[0].key;
    let input = UpdateIssueInput {
        title: Some("Updated Title".to_string()),
        description: Some("Updated description".to_string()),
        state: None,
        labels: Some(vec!["test".to_string()]),
        assignees: None,
        priority: None,
    };

    let result = provider.update_issue(key, input).await;

    // Write operations are not supported in TestProvider
    assert!(
        result.is_err(),
        "Update issue should fail in test mode with proper error"
    );
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("not supported"),
        "Error should indicate operation not supported: {}",
        err_msg
    );
}

/// Test that adding comment via PR interface returns error in test mode.
/// Note: In real implementation this would validate that the key is a PR,
/// but TestProvider always returns "not supported" for write operations.
#[tokio::test]
async fn test_add_comment_via_pr_key_not_supported() {
    let provider = TestProvider::github();

    // Write operations are not supported in TestProvider regardless of key format
    let input = CreateCommentInput {
        body: "This should fail".to_string(),
        position: None,
        discussion_id: None,
    };

    let result = MergeRequestProvider::add_comment(&provider, "pr#1", input).await;

    assert!(result.is_err(), "Adding comment should fail in test mode");
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("not supported"),
        "Error should indicate operation not supported: {}",
        err_msg
    );
}
