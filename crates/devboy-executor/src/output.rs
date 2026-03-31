use devboy_core::{
    Comment, Discussion, FileDiff, Issue, IssueRelations, IssueStatus, JobLogOutput, MeetingNote,
    MeetingTranscript, MergeRequest, Pagination, PipelineInfo, SortInfo, User,
};

/// Metadata from provider result (pagination + sort info).
#[derive(Debug, Clone, Default)]
pub struct ResultMeta {
    pub pagination: Option<Pagination>,
    pub sort_info: Option<SortInfo>,
}

/// Typed result of tool execution.
///
/// Each variant carries structured data from the provider.
/// The caller (MCP server, NAPI bridge, HTTP handler) decides
/// how to format the output (pipeline text, JSON, etc.).
#[derive(Debug)]
pub enum ToolOutput {
    /// List of merge requests / pull requests
    MergeRequests(Vec<MergeRequest>, Option<ResultMeta>),
    /// Single merge request / pull request
    SingleMergeRequest(Box<MergeRequest>),
    /// MR/PR discussions with comments and code positions
    Discussions(Vec<Discussion>, Option<ResultMeta>),
    /// File diffs from a merge request / pull request
    Diffs(Vec<FileDiff>, Option<ResultMeta>),
    /// List of issues / tasks
    Issues(Vec<Issue>, Option<ResultMeta>),
    /// Single issue / task
    SingleIssue(Box<Issue>),
    /// Comments on an issue or merge request
    Comments(Vec<Comment>, Option<ResultMeta>),
    /// CI/CD pipeline status with jobs
    Pipeline(Box<PipelineInfo>),
    /// Job log output
    JobLog(Box<JobLogOutput>),
    /// Available issue statuses
    Statuses(Vec<IssueStatus>, Option<ResultMeta>),
    /// List of users
    Users(Vec<User>, Option<ResultMeta>),
    /// List of meeting notes
    MeetingNotes(Vec<MeetingNote>, Option<ResultMeta>),
    /// Single meeting transcript with sentences
    MeetingTranscript(Box<MeetingTranscript>),
    /// Issue relations (parent, subtasks, linked issues)
    Relations(Box<IssueRelations>),
    /// Plain text result (e.g., "Comment created successfully")
    Text(String),
}

impl ToolOutput {
    /// Returns the number of items in collection outputs, or 1 for single items.
    pub fn item_count(&self) -> usize {
        match self {
            Self::MergeRequests(v, _) => v.len(),
            Self::Discussions(v, _) => v.len(),
            Self::Diffs(v, _) => v.len(),
            Self::Issues(v, _) => v.len(),
            Self::Comments(v, _) => v.len(),
            Self::Statuses(v, _) => v.len(),
            Self::Users(v, _) => v.len(),
            Self::MeetingNotes(v, _) => v.len(),
            Self::SingleMergeRequest(_)
            | Self::SingleIssue(_)
            | Self::Pipeline(_)
            | Self::JobLog(_)
            | Self::MeetingTranscript(_)
            | Self::Relations(_)
            | Self::Text(_) => 1,
        }
    }

    /// Returns a human-readable type name for this output.
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::MergeRequests(..) => "merge_requests",
            Self::SingleMergeRequest(_) => "merge_request",
            Self::Discussions(..) => "discussions",
            Self::Diffs(..) => "diffs",
            Self::Issues(..) => "issues",
            Self::SingleIssue(_) => "issue",
            Self::Comments(..) => "comments",
            Self::Pipeline(_) => "pipeline",
            Self::JobLog(_) => "job_log",
            Self::Statuses(..) => "statuses",
            Self::Users(..) => "users",
            Self::MeetingNotes(..) => "meeting_notes",
            Self::MeetingTranscript(_) => "meeting_transcript",
            Self::Relations(_) => "issue_relations",
            Self::Text(_) => "text",
        }
    }

    /// Returns the result metadata (pagination + sort info) if present.
    pub fn result_meta(&self) -> Option<&ResultMeta> {
        match self {
            Self::MergeRequests(_, meta)
            | Self::Discussions(_, meta)
            | Self::Diffs(_, meta)
            | Self::Issues(_, meta)
            | Self::Comments(_, meta)
            | Self::Statuses(_, meta)
            | Self::Users(_, meta)
            | Self::MeetingNotes(_, meta) => meta.as_ref(),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use devboy_core::{Issue, IssueStatus, MergeRequest, User};

    fn issue() -> Issue {
        Issue {
            key: "gh#1".into(),
            title: "T".into(),
            description: None,
            state: "open".into(),
            source: "mock".into(),
            priority: None,
            labels: vec![],
            author: None,
            assignees: vec![],
            url: None,
            created_at: None,
            updated_at: None,
            parent: None,
            subtasks: vec![],
        }
    }

    fn mr() -> MergeRequest {
        MergeRequest {
            key: "pr#1".into(),
            title: "T".into(),
            description: None,
            state: "open".into(),
            source: "mock".into(),
            source_branch: "f".into(),
            target_branch: "m".into(),
            author: None,
            assignees: vec![],
            reviewers: vec![],
            labels: vec![],
            draft: false,
            url: None,
            created_at: None,
            updated_at: None,
        }
    }

    #[test]
    fn test_item_count_all_variants() {
        assert_eq!(
            ToolOutput::Issues(vec![issue(), issue()], None).item_count(),
            2
        );
        assert_eq!(ToolOutput::MergeRequests(vec![], None).item_count(), 0);
        assert_eq!(ToolOutput::SingleIssue(Box::new(issue())).item_count(), 1);
        assert_eq!(
            ToolOutput::SingleMergeRequest(Box::new(mr())).item_count(),
            1
        );
        assert_eq!(ToolOutput::Discussions(vec![], None).item_count(), 0);
        assert_eq!(ToolOutput::Diffs(vec![], None).item_count(), 0);
        assert_eq!(ToolOutput::Comments(vec![], None).item_count(), 0);
        assert_eq!(
            ToolOutput::Pipeline(Box::new(devboy_core::PipelineInfo {
                id: "1".into(),
                status: devboy_core::PipelineStatus::Success,
                reference: "main".into(),
                sha: "abc".into(),
                url: None,
                duration: None,
                coverage: None,
                summary: devboy_core::PipelineSummary::default(),
                stages: vec![],
                failed_jobs: vec![],
            }))
            .item_count(),
            1
        );
        assert_eq!(
            ToolOutput::JobLog(Box::new(devboy_core::JobLogOutput {
                job_id: "1".into(),
                job_name: None,
                content: "log".into(),
                mode: "smart".into(),
                total_lines: None,
            }))
            .item_count(),
            1
        );
        assert_eq!(
            ToolOutput::Statuses(
                vec![IssueStatus {
                    id: "1".into(),
                    name: "Open".into(),
                    category: "open".into(),
                    color: None,
                    order: None,
                }],
                None
            )
            .item_count(),
            1
        );
        assert_eq!(ToolOutput::Statuses(vec![], None).item_count(), 0);
        assert_eq!(
            ToolOutput::Users(
                vec![User {
                    id: "1".into(),
                    username: "test".into(),
                    name: None,
                    email: None,
                    avatar_url: None,
                }],
                None
            )
            .item_count(),
            1
        );
        assert_eq!(ToolOutput::Users(vec![], None).item_count(), 0);
        assert_eq!(ToolOutput::Relations(Box::default()).item_count(), 1);
        assert_eq!(ToolOutput::Text("x".into()).item_count(), 1);
    }

    #[test]
    fn test_type_name_all_variants() {
        assert_eq!(ToolOutput::Issues(vec![], None).type_name(), "issues");
        assert_eq!(
            ToolOutput::MergeRequests(vec![], None).type_name(),
            "merge_requests"
        );
        assert_eq!(
            ToolOutput::SingleIssue(Box::new(issue())).type_name(),
            "issue"
        );
        assert_eq!(
            ToolOutput::SingleMergeRequest(Box::new(mr())).type_name(),
            "merge_request"
        );
        assert_eq!(
            ToolOutput::Discussions(vec![], None).type_name(),
            "discussions"
        );
        assert_eq!(ToolOutput::Diffs(vec![], None).type_name(), "diffs");
        assert_eq!(ToolOutput::Comments(vec![], None).type_name(), "comments");
        assert_eq!(
            ToolOutput::Pipeline(Box::new(devboy_core::PipelineInfo {
                id: "1".into(),
                status: devboy_core::PipelineStatus::Success,
                reference: "main".into(),
                sha: "abc".into(),
                url: None,
                duration: None,
                coverage: None,
                summary: devboy_core::PipelineSummary::default(),
                stages: vec![],
                failed_jobs: vec![],
            }))
            .type_name(),
            "pipeline"
        );
        assert_eq!(
            ToolOutput::JobLog(Box::new(devboy_core::JobLogOutput {
                job_id: "1".into(),
                job_name: None,
                content: "log".into(),
                mode: "smart".into(),
                total_lines: None,
            }))
            .type_name(),
            "job_log"
        );
        assert_eq!(ToolOutput::Statuses(vec![], None).type_name(), "statuses");
        assert_eq!(ToolOutput::Users(vec![], None).type_name(), "users");
        assert_eq!(
            ToolOutput::Relations(Box::default()).type_name(),
            "issue_relations"
        );
        assert_eq!(ToolOutput::Text("x".into()).type_name(), "text");
    }

    #[test]
    fn test_result_meta_present() {
        let meta = ResultMeta {
            pagination: Some(devboy_core::Pagination {
                offset: 0,
                limit: 10,
                total: Some(50),
                has_more: true,
            }),
            sort_info: Some(devboy_core::SortInfo {
                current_sort: Some("created_at:desc".into()),
                available_sorts: vec!["created_at".into()],
            }),
        };

        let output = ToolOutput::Issues(vec![issue()], Some(meta));
        let rm = output.result_meta().unwrap();
        assert!(rm.pagination.is_some());
        assert!(rm.sort_info.is_some());
        assert_eq!(rm.pagination.as_ref().unwrap().total, Some(50));
    }

    #[test]
    fn test_result_meta_none_for_single_variants() {
        assert!(
            ToolOutput::SingleIssue(Box::new(issue()))
                .result_meta()
                .is_none()
        );
        assert!(
            ToolOutput::SingleMergeRequest(Box::new(mr()))
                .result_meta()
                .is_none()
        );
        assert!(ToolOutput::Text("hello".into()).result_meta().is_none());
        assert!(
            ToolOutput::Relations(Box::default())
                .result_meta()
                .is_none()
        );
    }

    #[test]
    fn test_result_meta_none_when_not_provided() {
        assert!(ToolOutput::Issues(vec![], None).result_meta().is_none());
        assert!(
            ToolOutput::MergeRequests(vec![], None)
                .result_meta()
                .is_none()
        );
        assert!(
            ToolOutput::Discussions(vec![], None)
                .result_meta()
                .is_none()
        );
        assert!(ToolOutput::Diffs(vec![], None).result_meta().is_none());
        assert!(ToolOutput::Comments(vec![], None).result_meta().is_none());
        assert!(ToolOutput::Statuses(vec![], None).result_meta().is_none());
        assert!(ToolOutput::Users(vec![], None).result_meta().is_none());
        assert!(
            ToolOutput::MeetingNotes(vec![], None)
                .result_meta()
                .is_none()
        );
    }
}
