//! Trim tree (TrimTree) for budget trimming.
//!
//! The JSON response is modeled as a rooted tree T = (V, E) where each node has:
//! - weight -- cost in tokens (serialization without children)
//! - value -- information value (depends on the strategy)
//! - included -- whether the node is included in the final output
//!
//! Builders convert typed data into TrimNode trees with precomputed weights.

use devboy_core::{Comment, Discussion, FileDiff, Issue, MergeRequest};

use crate::token_counter::estimate_tokens;
use crate::toon;

/// Trim tree node.
#[derive(Debug, Clone)]
pub struct TrimNode {
    /// Unique node identifier within the tree
    pub id: usize,
    /// Node type
    pub kind: NodeKind,
    /// Weight in tokens (serialization cost without children)
    pub weight: usize,
    /// Information value (0.0-1.0), assigned by the strategy
    pub value: f64,
    /// Child nodes
    pub children: Vec<TrimNode>,
    /// Whether the node is included in the final output
    pub included: bool,
}

/// Node type in the trim tree.
#[derive(Debug, Clone, PartialEq)]
pub enum NodeKind {
    /// Root node (collection)
    Root,
    /// Collection element (issue, MR, comment, diff)
    Item {
        /// Index of the element in the original array
        index: usize,
    },
    /// Field within an element (description, body, diff content)
    Field {
        /// Field name
        name: String,
    },
    /// Text block (description body, diff content, log content)
    Text,
}

impl TrimNode {
    /// Create a new node.
    pub fn new(id: usize, kind: NodeKind, weight: usize) -> Self {
        Self {
            id,
            kind,
            weight,
            value: 1.0, // default value, overridden by the strategy
            children: Vec::new(),
            included: true,
        }
    }

    /// Total number of nodes in the subtree (including self).
    pub fn count_nodes(&self) -> usize {
        1 + self.children.iter().map(|c| c.count_nodes()).sum::<usize>()
    }

    /// Total weight of the subtree (sum of weights of all included nodes).
    pub fn total_weight(&self) -> usize {
        if !self.included {
            return 0;
        }
        self.weight
            + self
                .children
                .iter()
                .map(|c| c.total_weight())
                .sum::<usize>()
    }

    /// Total value of the subtree (sum of value * weight for included nodes).
    pub fn total_value(&self) -> f64 {
        if !self.included {
            return 0.0;
        }
        self.value * self.weight as f64 + self.children.iter().map(|c| c.total_value()).sum::<f64>()
    }

    /// Information density: value / weight.
    pub fn density(&self) -> f64 {
        if self.weight == 0 {
            return 0.0;
        }
        self.value / self.weight as f64
    }

    /// Count of included Item nodes.
    pub fn included_items_count(&self) -> usize {
        let self_count = if self.included && matches!(self.kind, NodeKind::Item { .. }) {
            1
        } else {
            0
        };
        self_count
            + self
                .children
                .iter()
                .map(|c| c.included_items_count())
                .sum::<usize>()
    }

    /// Collect indices of included Item nodes.
    pub fn included_item_indices(&self) -> Vec<usize> {
        let mut indices = Vec::new();
        self.collect_included_indices(&mut indices);
        indices
    }

    fn collect_included_indices(&self, indices: &mut Vec<usize>) {
        if self.included
            && let NodeKind::Item { index } = &self.kind
        {
            indices.push(*index);
        }
        if self.included {
            for child in &self.children {
                child.collect_included_indices(indices);
            }
        }
    }

    /// Collect indices of excluded Item nodes (overflow).
    pub fn excluded_item_indices(&self) -> Vec<usize> {
        let mut indices = Vec::new();
        self.collect_excluded_indices(&mut indices);
        indices
    }

    fn collect_excluded_indices(&self, indices: &mut Vec<usize>) {
        if !self.included
            && let NodeKind::Item { index } = &self.kind
        {
            indices.push(*index);
            // All descendants of an excluded node are also excluded
        } else if self.included {
            for child in &self.children {
                child.collect_excluded_indices(indices);
            }
        }
    }
}

// ============================================================================
// ID Generator
// ============================================================================

/// Unique ID generator for tree nodes.
struct IdGen(usize);

impl IdGen {
    fn new() -> Self {
        Self(0)
    }

    fn next(&mut self) -> usize {
        let id = self.0;
        self.0 += 1;
        id
    }
}

// ============================================================================
// Builders
// ============================================================================

/// Build a tree from a list of issues.
///
/// Structure: Root -> [Item(0), Item(1), ...].
/// Each Item may contain a Field("description") if the description is long.
pub fn build_issues_tree(issues: &[Issue]) -> TrimNode {
    let mut id_gen = IdGen::new();
    let mut root = TrimNode::new(id_gen.next(), NodeKind::Root, 0);

    for (i, issue) in issues.iter().enumerate() {
        let item_weight = estimate_item_tokens(issue);
        let mut item = TrimNode::new(id_gen.next(), NodeKind::Item { index: i }, item_weight);

        // If description > 100 chars -- extract into a separate Field for granular trimming
        if let Some(desc) = &issue.description
            && desc.len() > 100
        {
            let desc_weight = estimate_tokens(desc);
            item.weight = item.weight.saturating_sub(desc_weight);
            let field = TrimNode::new(
                id_gen.next(),
                NodeKind::Field {
                    name: "description".into(),
                },
                desc_weight,
            );
            item.children.push(field);
        }

        root.children.push(item);
    }

    root
}

/// Build a tree from a list of merge requests.
pub fn build_merge_requests_tree(mrs: &[MergeRequest]) -> TrimNode {
    let mut id_gen = IdGen::new();
    let mut root = TrimNode::new(id_gen.next(), NodeKind::Root, 0);

    for (i, mr) in mrs.iter().enumerate() {
        let item_weight = estimate_item_tokens(mr);
        let mut item = TrimNode::new(id_gen.next(), NodeKind::Item { index: i }, item_weight);

        if let Some(desc) = &mr.description
            && desc.len() > 100
        {
            let desc_weight = estimate_tokens(desc);
            item.weight = item.weight.saturating_sub(desc_weight);
            let field = TrimNode::new(
                id_gen.next(),
                NodeKind::Field {
                    name: "description".into(),
                },
                desc_weight,
            );
            item.children.push(field);
        }

        root.children.push(item);
    }

    root
}

/// Build a tree from a list of file diffs.
///
/// Diff content is extracted into Field("diff") for granular trimming by file type.
pub fn build_diffs_tree(diffs: &[FileDiff]) -> TrimNode {
    let mut id_gen = IdGen::new();
    let mut root = TrimNode::new(id_gen.next(), NodeKind::Root, 0);

    for (i, diff) in diffs.iter().enumerate() {
        let item_weight = estimate_item_tokens(diff);
        let mut item = TrimNode::new(id_gen.next(), NodeKind::Item { index: i }, item_weight);

        // Diff content is always extracted into a separate node
        if !diff.diff.is_empty() {
            let diff_weight = estimate_tokens(&diff.diff);
            item.weight = item.weight.saturating_sub(diff_weight);
            let field = TrimNode::new(
                id_gen.next(),
                NodeKind::Field {
                    name: "diff".into(),
                },
                diff_weight,
            );
            item.children.push(field);
        }

        root.children.push(item);
    }

    root
}

/// Build a tree from a list of comments.
pub fn build_comments_tree(comments: &[Comment]) -> TrimNode {
    let mut id_gen = IdGen::new();
    let mut root = TrimNode::new(id_gen.next(), NodeKind::Root, 0);

    for (i, comment) in comments.iter().enumerate() {
        let item_weight = estimate_item_tokens(comment);
        let mut item = TrimNode::new(id_gen.next(), NodeKind::Item { index: i }, item_weight);

        // Body > 200 chars -- extract into a separate node
        if comment.body.len() > 200 {
            let body_weight = estimate_tokens(&comment.body);
            item.weight = item.weight.saturating_sub(body_weight);
            let field = TrimNode::new(
                id_gen.next(),
                NodeKind::Field {
                    name: "body".into(),
                },
                body_weight,
            );
            item.children.push(field);
        }

        root.children.push(item);
    }

    root
}

/// Build a tree from a list of discussions.
///
/// Two-level structure: Root -> Discussion(Item) -> Comment(Item).
pub fn build_discussions_tree(discussions: &[Discussion]) -> TrimNode {
    let mut id_gen = IdGen::new();
    let mut root = TrimNode::new(id_gen.next(), NodeKind::Root, 0);

    for (i, discussion) in discussions.iter().enumerate() {
        // Discussion weight = metadata (id, resolved, position) without comments
        let metadata_weight = estimate_tokens(&format!(
            "id:{} resolved:{}",
            discussion.id, discussion.resolved
        ));
        let mut disc_node =
            TrimNode::new(id_gen.next(), NodeKind::Item { index: i }, metadata_weight);

        // Each comment is a separate child
        for (j, comment) in discussion.comments.iter().enumerate() {
            let comment_weight = estimate_item_tokens(comment);
            let comment_node =
                TrimNode::new(id_gen.next(), NodeKind::Item { index: j }, comment_weight);
            disc_node.children.push(comment_node);
        }

        root.children.push(disc_node);
    }

    root
}

/// Estimate tokens for a single element via TOON encode.
fn estimate_item_tokens<T: serde::Serialize>(item: &T) -> usize {
    match toon::encode_value(item) {
        Ok(encoded) => estimate_tokens(&encoded),
        Err(_) => {
            // Fallback: estimate via JSON
            match serde_json::to_string(item) {
                Ok(json) => estimate_tokens(&json),
                Err(_) => 50, // minimum estimate
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use devboy_core::User;

    fn sample_issues(n: usize) -> Vec<Issue> {
        (0..n)
            .map(|i| Issue {
                key: format!("gh#{}", i + 1),
                title: format!("Issue {}", i + 1),
                description: if i % 2 == 0 {
                    Some("A".repeat(200)) // long description
                } else {
                    Some("Short desc".into())
                },
                state: "open".into(),
                source: "github".into(),
                priority: None,
                labels: vec!["bug".into()],
                author: Some(User {
                    id: format!("{}", i),
                    username: format!("user{}", i),
                    name: None,
                    email: None,
                    avatar_url: None,
                }),
                assignees: vec![],
                url: Some(format!("https://github.com/test/repo/issues/{}", i + 1)),
                created_at: Some("2024-01-01T00:00:00Z".into()),
                updated_at: Some("2024-01-02T00:00:00Z".into()),
                parent: None,
                subtasks: vec![],
            })
            .collect()
    }

    fn sample_diffs(n: usize) -> Vec<FileDiff> {
        (0..n)
            .map(|i| FileDiff {
                file_path: format!("src/file_{}.rs", i),
                old_path: None,
                new_file: false,
                deleted_file: false,
                renamed_file: false,
                diff: format!("+added line {}\n-removed line {}", i, i),
                additions: Some(1),
                deletions: Some(1),
            })
            .collect()
    }

    fn sample_comments(n: usize) -> Vec<Comment> {
        (0..n)
            .map(|i| Comment {
                id: format!("{}", i),
                body: format!("Comment body {}", i),
                author: None,
                created_at: Some("2024-01-01T00:00:00Z".into()),
                updated_at: None,
                position: None,
            })
            .collect()
    }

    fn sample_discussions(n: usize) -> Vec<Discussion> {
        (0..n)
            .map(|i| Discussion {
                id: format!("{}", i),
                resolved: i % 2 == 0,
                resolved_by: None,
                comments: vec![
                    Comment {
                        id: format!("c{}a", i),
                        body: format!("First comment in discussion {}", i),
                        author: None,
                        created_at: None,
                        updated_at: None,
                        position: None,
                    },
                    Comment {
                        id: format!("c{}b", i),
                        body: format!("Reply in discussion {}", i),
                        author: None,
                        created_at: None,
                        updated_at: None,
                        position: None,
                    },
                ],
                position: None,
            })
            .collect()
    }

    // --- Structure tests ---

    #[test]
    fn test_build_issues_tree_structure() {
        let issues = sample_issues(5);
        let tree = build_issues_tree(&issues);

        assert_eq!(tree.kind, NodeKind::Root);
        assert_eq!(tree.children.len(), 5);
        assert!(tree.weight == 0); // root has no own weight

        // Each child is an Item
        for (i, child) in tree.children.iter().enumerate() {
            assert_eq!(child.kind, NodeKind::Item { index: i });
            assert!(child.weight > 0);
            assert!(child.included);
        }
    }

    #[test]
    fn test_build_issues_tree_with_description_fields() {
        let issues = sample_issues(4);
        let tree = build_issues_tree(&issues);

        // Issues 0, 2 have long description → Field child
        assert!(
            !tree.children[0].children.is_empty(),
            "Issue 0 should have description field"
        );
        assert!(
            tree.children[1].children.is_empty(),
            "Issue 1 should not have description field (short)"
        );
        assert!(!tree.children[2].children.is_empty());
        assert!(tree.children[3].children.is_empty());
    }

    #[test]
    fn test_build_diffs_tree_structure() {
        let diffs = sample_diffs(3);
        let tree = build_diffs_tree(&diffs);

        assert_eq!(tree.children.len(), 3);
        // Each diff has diff content as a Field child
        for child in &tree.children {
            assert_eq!(child.children.len(), 1);
            assert_eq!(
                child.children[0].kind,
                NodeKind::Field {
                    name: "diff".into()
                }
            );
        }
    }

    #[test]
    fn test_build_comments_tree_structure() {
        let comments = sample_comments(5);
        let tree = build_comments_tree(&comments);

        assert_eq!(tree.children.len(), 5);
        // Short comments — no child fields
        for child in &tree.children {
            assert!(child.children.is_empty());
        }
    }

    #[test]
    fn test_build_discussions_tree_structure() {
        let discussions = sample_discussions(3);
        let tree = build_discussions_tree(&discussions);

        assert_eq!(tree.children.len(), 3);
        // Each discussion has 2 comment children
        for disc in &tree.children {
            assert_eq!(disc.children.len(), 2);
        }
    }

    #[test]
    fn test_build_merge_requests_tree() {
        let mrs: Vec<MergeRequest> = (0..3)
            .map(|i| MergeRequest {
                key: format!("pr#{}", i),
                title: format!("PR {}", i),
                description: Some("A".repeat(200)),
                state: "open".into(),
                source: "github".into(),
                source_branch: "feat".into(),
                target_branch: "main".into(),
                author: None,
                assignees: vec![],
                reviewers: vec![],
                labels: vec![],
                draft: false,
                url: None,
                created_at: None,
                updated_at: None,
            })
            .collect();

        let tree = build_merge_requests_tree(&mrs);
        assert_eq!(tree.children.len(), 3);
        // Long description → Field child
        for child in &tree.children {
            assert!(!child.children.is_empty());
        }
    }

    // --- Counting tests ---

    #[test]
    fn test_count_nodes() {
        let issues = sample_issues(5);
        let tree = build_issues_tree(&issues);

        // Root + 5 items + some description fields
        assert!(tree.count_nodes() >= 6);
    }

    #[test]
    fn test_total_weight() {
        let issues = sample_issues(3);
        let tree = build_issues_tree(&issues);

        let total = tree.total_weight();
        assert!(total > 0);

        // Weight should be sum of all children
        let manual_sum: usize = tree.children.iter().map(|c| c.total_weight()).sum();
        assert_eq!(total, manual_sum); // root weight is 0
    }

    #[test]
    fn test_included_items_count() {
        let issues = sample_issues(5);
        let mut tree = build_issues_tree(&issues);

        assert_eq!(tree.included_items_count(), 5);

        // Exclude 2 items
        tree.children[1].included = false;
        tree.children[3].included = false;

        assert_eq!(tree.included_items_count(), 3);
    }

    #[test]
    fn test_included_excluded_indices() {
        let issues = sample_issues(5);
        let mut tree = build_issues_tree(&issues);

        tree.children[1].included = false;
        tree.children[3].included = false;

        let included = tree.included_item_indices();
        let excluded = tree.excluded_item_indices();

        assert_eq!(included, vec![0, 2, 4]);
        assert_eq!(excluded, vec![1, 3]);
    }

    // --- Weight correctness ---

    #[test]
    fn test_weights_are_positive() {
        let issues = sample_issues(10);
        let tree = build_issues_tree(&issues);

        for child in &tree.children {
            assert!(
                child.weight > 0 || !child.children.is_empty(),
                "Item should have weight or children with weight"
            );
            assert!(child.total_weight() > 0);
        }
    }

    #[test]
    fn test_total_weight_decreases_when_excluded() {
        let issues = sample_issues(5);
        let mut tree = build_issues_tree(&issues);

        let full_weight = tree.total_weight();
        tree.children[0].included = false;
        let reduced_weight = tree.total_weight();

        assert!(reduced_weight < full_weight);
    }

    // --- Density ---

    #[test]
    fn test_density_calculation() {
        let mut node = TrimNode::new(0, NodeKind::Item { index: 0 }, 100);
        node.value = 0.5;
        assert!((node.density() - 0.005).abs() < 0.0001);

        let zero_node = TrimNode::new(1, NodeKind::Item { index: 1 }, 0);
        assert_eq!(zero_node.density(), 0.0);
    }
}
