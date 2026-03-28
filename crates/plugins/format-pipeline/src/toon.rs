//! TOON (Token-Oriented Object Notation) encoding for tool output.
//!
//! Wrappers around `toon_format::encode` with optimal settings for LLM.
//! Supports three detail levels (TrimLevel) for controlling
//! the number of fields during budget trimming.

use devboy_core::{Comment, Discussion, FileDiff, Issue, MergeRequest, Result};
use serde::Serialize;
use toon_format::EncodeOptions;
use toon_format::types::KeyFoldingMode;

/// Detail level for TOON encoding.
///
/// Controls which fields are included in the output.
/// Used by budget pipeline for progressive detail reduction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrimLevel {
    /// All fields (~750 tokens/issue)
    Full,
    /// Core fields, without timestamps and avatar_url (~400 tokens/issue)
    Standard,
    /// Only key fields: key, title, state (~150 tokens/issue)
    Minimal,
}

/// Optimal TOON settings for LLM: minimal indent, key folding.
fn default_opts() -> EncodeOptions {
    EncodeOptions::new()
        .with_spaces(1)
        .with_key_folding(KeyFoldingMode::Safe)
}

/// Encode any Serialize type into TOON.
pub fn encode_value<T: Serialize>(value: &T) -> Result<String> {
    toon_format::encode(value, &default_opts())
        .map_err(|e| devboy_core::Error::Other(anyhow::anyhow!("TOON encode: {e}")))
}

/// Encode an array of issues into TOON with the specified detail level.
pub fn encode_issues(issues: &[Issue], level: TrimLevel) -> Result<String> {
    match level {
        TrimLevel::Full => encode_value(&issues),
        TrimLevel::Standard => {
            let views: Vec<IssueStandard> = issues.iter().map(IssueStandard::from).collect();
            encode_value(&views)
        }
        TrimLevel::Minimal => {
            let views: Vec<IssueMinimal> = issues.iter().map(IssueMinimal::from).collect();
            encode_value(&views)
        }
    }
}

/// Encode an array of merge requests into TOON with the specified detail level.
pub fn encode_merge_requests(mrs: &[MergeRequest], level: TrimLevel) -> Result<String> {
    match level {
        TrimLevel::Full => encode_value(&mrs),
        TrimLevel::Standard => {
            let views: Vec<MrStandard> = mrs.iter().map(MrStandard::from).collect();
            encode_value(&views)
        }
        TrimLevel::Minimal => {
            let views: Vec<MrMinimal> = mrs.iter().map(MrMinimal::from).collect();
            encode_value(&views)
        }
    }
}

/// Encode an array of file diffs into TOON.
pub fn encode_diffs(diffs: &[FileDiff]) -> Result<String> {
    encode_value(&diffs)
}

/// Encode an array of comments into TOON.
pub fn encode_comments(comments: &[Comment]) -> Result<String> {
    encode_value(&comments)
}

/// Encode an array of discussions into TOON.
pub fn encode_discussions(discussions: &[Discussion]) -> Result<String> {
    encode_value(&discussions)
}

// ============================================================================
// View structs for Standard and Minimal levels
// ============================================================================

/// Issue at Standard level -- without timestamps and avatar.
#[derive(Serialize)]
struct IssueStandard<'a> {
    key: &'a str,
    title: &'a str,
    state: &'a str,
    source: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    priority: Option<&'a str>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    labels: &'a Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    author: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    url: Option<&'a str>,
}

impl<'a> From<&'a Issue> for IssueStandard<'a> {
    fn from(i: &'a Issue) -> Self {
        Self {
            key: &i.key,
            title: &i.title,
            state: &i.state,
            source: &i.source,
            priority: i.priority.as_deref(),
            labels: &i.labels,
            description: i.description.as_deref(),
            author: i.author.as_ref().map(|u| u.username.as_str()),
            url: i.url.as_deref(),
        }
    }
}

/// Issue at Minimal level -- only key, title, state.
#[derive(Serialize)]
struct IssueMinimal<'a> {
    key: &'a str,
    title: &'a str,
    state: &'a str,
}

impl<'a> From<&'a Issue> for IssueMinimal<'a> {
    fn from(i: &'a Issue) -> Self {
        Self {
            key: &i.key,
            title: &i.title,
            state: &i.state,
        }
    }
}

/// MergeRequest at Standard level.
#[derive(Serialize)]
struct MrStandard<'a> {
    key: &'a str,
    title: &'a str,
    state: &'a str,
    source: &'a str,
    source_branch: &'a str,
    target_branch: &'a str,
    draft: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    labels: &'a Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    author: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    url: Option<&'a str>,
}

impl<'a> From<&'a MergeRequest> for MrStandard<'a> {
    fn from(mr: &'a MergeRequest) -> Self {
        Self {
            key: &mr.key,
            title: &mr.title,
            state: &mr.state,
            source: &mr.source,
            source_branch: &mr.source_branch,
            target_branch: &mr.target_branch,
            draft: mr.draft,
            labels: &mr.labels,
            description: mr.description.as_deref(),
            author: mr.author.as_ref().map(|u| u.username.as_str()),
            url: mr.url.as_deref(),
        }
    }
}

/// MergeRequest at Minimal level.
#[derive(Serialize)]
struct MrMinimal<'a> {
    key: &'a str,
    title: &'a str,
    state: &'a str,
    source_branch: &'a str,
    target_branch: &'a str,
}

impl<'a> From<&'a MergeRequest> for MrMinimal<'a> {
    fn from(mr: &'a MergeRequest) -> Self {
        Self {
            key: &mr.key,
            title: &mr.title,
            state: &mr.state,
            source_branch: &mr.source_branch,
            target_branch: &mr.target_branch,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use devboy_core::User;

    fn sample_issue() -> Issue {
        Issue {
            key: "gh#1".into(),
            title: "Fix login bug".into(),
            description: Some("Users cannot login with SSO".into()),
            state: "open".into(),
            source: "github".into(),
            priority: Some("high".into()),
            labels: vec!["bug".into(), "auth".into()],
            author: Some(User {
                id: "1".into(),
                username: "alice".into(),
                name: Some("Alice Smith".into()),
                email: None,
                avatar_url: None,
            }),
            assignees: vec![],
            url: Some("https://github.com/test/repo/issues/1".into()),
            created_at: Some("2024-01-01T00:00:00Z".into()),
            updated_at: Some("2024-01-02T00:00:00Z".into()),
        }
    }

    fn sample_mr() -> MergeRequest {
        MergeRequest {
            key: "pr#42".into(),
            title: "Add SSO support".into(),
            description: Some("Implements SAML-based SSO".into()),
            state: "open".into(),
            source: "github".into(),
            source_branch: "feat/sso".into(),
            target_branch: "main".into(),
            author: Some(User {
                id: "2".into(),
                username: "bob".into(),
                name: None,
                email: None,
                avatar_url: None,
            }),
            assignees: vec![],
            reviewers: vec![],
            labels: vec!["feature".into()],
            draft: false,
            url: Some("https://github.com/test/repo/pull/42".into()),
            created_at: Some("2024-01-01T00:00:00Z".into()),
            updated_at: Some("2024-01-02T00:00:00Z".into()),
        }
    }

    #[test]
    fn test_encode_issues_full() {
        let issues = vec![sample_issue()];
        let result = encode_issues(&issues, TrimLevel::Full).unwrap();
        assert!(result.contains("gh#1"));
        assert!(result.contains("Fix login bug"));
        assert!(result.contains("2024-01-01")); // timestamps present in Full
    }

    #[test]
    fn test_encode_issues_standard() {
        let issues = vec![sample_issue()];
        let result = encode_issues(&issues, TrimLevel::Standard).unwrap();
        assert!(result.contains("gh#1"));
        assert!(result.contains("Fix login bug"));
        assert!(!result.contains("2024-01-01")); // no timestamps
        assert!(!result.contains("avatar")); // no avatar
    }

    #[test]
    fn test_encode_issues_minimal() {
        let issues = vec![sample_issue()];
        let result = encode_issues(&issues, TrimLevel::Minimal).unwrap();
        assert!(result.contains("gh#1"));
        assert!(result.contains("Fix login bug"));
        assert!(result.contains("open"));
        assert!(!result.contains("github")); // no source
        assert!(!result.contains("alice")); // no author
    }

    #[test]
    fn test_encode_merge_requests_full() {
        let mrs = vec![sample_mr()];
        let result = encode_merge_requests(&mrs, TrimLevel::Full).unwrap();
        assert!(result.contains("pr#42"));
        assert!(result.contains("Add SSO support"));
    }

    #[test]
    fn test_encode_merge_requests_standard() {
        let mrs = vec![sample_mr()];
        let result = encode_merge_requests(&mrs, TrimLevel::Standard).unwrap();
        assert!(result.contains("pr#42"));
        assert!(result.contains("Add SSO support"));
        assert!(result.contains("feat/sso"));
        assert!(!result.contains("2024-01-01")); // no timestamps
    }

    #[test]
    fn test_encode_merge_requests_minimal() {
        let mrs = vec![sample_mr()];
        let result = encode_merge_requests(&mrs, TrimLevel::Minimal).unwrap();
        assert!(result.contains("pr#42"));
        assert!(result.contains("feat/sso"));
        assert!(!result.contains("bob"));
    }

    #[test]
    fn test_encode_diffs() {
        let diffs = vec![FileDiff {
            file_path: "src/main.rs".into(),
            old_path: None,
            new_file: false,
            deleted_file: false,
            renamed_file: false,
            diff: "+added line\n-removed line".into(),
            additions: Some(1),
            deletions: Some(1),
        }];
        let result = encode_diffs(&diffs).unwrap();
        assert!(result.contains("src/main.rs"));
        assert!(result.contains("added line"));
    }

    #[test]
    fn test_encode_comments() {
        let comments = vec![Comment {
            id: "c1".into(),
            body: "LGTM!".into(),
            author: None,
            created_at: None,
            updated_at: None,
            position: None,
        }];
        let result = encode_comments(&comments).unwrap();
        assert!(result.contains("LGTM!"));
    }

    #[test]
    fn test_encode_discussions() {
        let discussions = vec![Discussion {
            id: "d1".into(),
            resolved: false,
            resolved_by: None,
            comments: vec![Comment {
                id: "c1".into(),
                body: "Needs review".into(),
                author: None,
                created_at: None,
                updated_at: None,
                position: None,
            }],
            position: None,
        }];
        let result = encode_discussions(&discussions).unwrap();
        assert!(result.contains("Needs review"));
    }

    #[test]
    fn test_toon_smaller_than_json() {
        let issues: Vec<Issue> = (1..=10)
            .map(|i| Issue {
                key: format!("gh#{i}"),
                title: format!("Issue {i}"),
                description: Some(format!("Description for issue {i}")),
                state: "open".into(),
                source: "github".into(),
                priority: None,
                labels: vec!["bug".into()],
                author: Some(User {
                    id: format!("{i}"),
                    username: format!("user{i}"),
                    name: None,
                    email: None,
                    avatar_url: None,
                }),
                assignees: vec![],
                url: Some(format!("https://github.com/test/repo/issues/{i}")),
                created_at: Some("2024-01-01T00:00:00Z".into()),
                updated_at: Some("2024-01-02T00:00:00Z".into()),
            })
            .collect();

        let json = serde_json::to_string_pretty(&issues).unwrap();
        let toon = encode_issues(&issues, TrimLevel::Full).unwrap();

        // TOON должен быть компактнее JSON
        assert!(
            toon.len() < json.len(),
            "TOON ({}) should be smaller than JSON ({})",
            toon.len(),
            json.len()
        );
    }

    #[test]
    fn test_minimal_much_smaller_than_full() {
        let issues: Vec<Issue> = (1..=5).map(|i| Issue {
            key: format!("gh#{i}"),
            title: format!("Issue {i}"),
            description: Some("A long description that takes many tokens and should be excluded in minimal mode".into()),
            state: "open".into(),
            source: "github".into(),
            priority: Some("high".into()),
            labels: vec!["bug".into(), "urgent".into()],
            author: Some(User {
                id: format!("{i}"),
                username: format!("user{i}"),
                name: Some(format!("User {i}")),
                email: Some(format!("user{i}@example.com")),
                avatar_url: Some("https://example.com/avatar.png".into()),
            }),
            assignees: vec![],
            url: Some(format!("https://github.com/test/repo/issues/{i}")),
            created_at: Some("2024-01-01T00:00:00Z".into()),
            updated_at: Some("2024-01-02T00:00:00Z".into()),
        }).collect();

        let full = encode_issues(&issues, TrimLevel::Full).unwrap();
        let minimal = encode_issues(&issues, TrimLevel::Minimal).unwrap();

        // Minimal должен быть значительно меньше Full
        assert!(
            minimal.len() * 3 < full.len(),
            "Minimal ({}) should be at least 3x smaller than Full ({})",
            minimal.len(),
            full.len()
        );
    }
}
