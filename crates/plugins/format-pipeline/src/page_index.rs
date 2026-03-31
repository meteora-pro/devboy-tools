//! Page index generation for large results.
//!
//! When budget trimming drops items, generates a structured index
//! describing what's on each page so the LLM can request specific pages.

use devboy_core::{Comment, Discussion, FileDiff, Issue, MergeRequest};
use std::collections::BTreeMap;

/// A single page descriptor in the index.
#[derive(Debug, Clone)]
pub struct PageDescriptor {
    /// Page number (1-based)
    pub page: usize,
    /// Human-readable summary of page contents
    pub summary: String,
    /// Number of items on this page
    pub item_count: usize,
    /// Offset to request this page
    pub offset: usize,
}

/// Full page index for a result set.
///
/// Note: there is no "current page" concept because budget trimming may
/// select non-contiguous items by priority. The shown items are selected
/// by the trim strategy, not by sequential page boundaries.
#[derive(Debug, Clone)]
pub struct PageIndex {
    /// Total items across all pages
    pub total_items: usize,
    /// Items shown (selected by budget trimming, may span multiple pages)
    pub shown_items: usize,
    /// Total number of pages
    pub total_pages: usize,
    /// Page descriptors
    pub pages: Vec<PageDescriptor>,
    /// Data type (e.g., "issues", "diffs", "discussions")
    pub data_type: String,
}

impl PageIndex {
    /// Render chunk index as a structured block.
    ///
    /// The output is designed to be read by an LLM agent that can decide
    /// which chunks to fetch based on the descriptions.
    pub fn to_toon(&self) -> String {
        let mut lines = Vec::new();
        lines.push(format!(
            "[chunks] {}/{} {} in {} chunks:",
            self.shown_items, self.total_items, self.data_type, self.total_pages
        ));
        for p in &self.pages {
            let marker = if p.page == 1 {
                " << included above"
            } else {
                ""
            };
            lines.push(format!(
                "  chunk {} (offset={}, limit={}): {}{}",
                p.page, p.offset, p.item_count, p.summary, marker
            ));
        }
        lines.push(
            "[/chunks] Use offset and limit to fetch any chunk. You may not need all chunks."
                .to_string(),
        );
        lines.join("\n")
    }
}

/// Default page size for chunking.
const DEFAULT_PAGE_SIZE: usize = 20;

/// Compute page size from the number of items that fit in budget.
///
/// Uses `included_items` (items that fit in one budget window) as page size.
/// Falls back to `DEFAULT_PAGE_SIZE` when included_items is 0.
fn compute_page_size(total_items: usize, included_items: usize) -> usize {
    if included_items > 0 {
        included_items
    } else {
        DEFAULT_PAGE_SIZE.min(total_items)
    }
}

// =============================================================================
// Type-specific page index builders
// =============================================================================

/// Build page index for issues.
pub fn build_issues_index(issues: &[Issue], included_count: usize) -> PageIndex {
    let total = issues.len();
    let page_size = compute_page_size(total, included_count);
    let total_pages = total.div_ceil(page_size);

    let pages: Vec<PageDescriptor> = (0..total_pages)
        .map(|page_idx| {
            let offset = page_idx * page_size;
            let end = (offset + page_size).min(total);
            let page_issues = &issues[offset..end];
            let item_count = page_issues.len();

            // Summarize: count by state
            let mut states: BTreeMap<&str, usize> = BTreeMap::new();
            for issue in page_issues {
                *states.entry(issue.state.as_str()).or_default() += 1;
            }
            let state_parts: Vec<String> =
                states.iter().map(|(s, c)| format!("{} {}", c, s)).collect();
            let summary = format!(
                "issues #{}-{} ({})",
                offset + 1,
                end,
                state_parts.join(", ")
            );

            PageDescriptor {
                page: page_idx + 1,
                summary,
                item_count,
                offset,
            }
        })
        .collect();

    PageIndex {
        total_items: total,
        shown_items: included_count,
        total_pages,
        pages,
        data_type: "issues".to_string(),
    }
}

/// Build page index for merge requests.
pub fn build_merge_requests_index(mrs: &[MergeRequest], included_count: usize) -> PageIndex {
    let total = mrs.len();
    let page_size = compute_page_size(total, included_count);
    let total_pages = total.div_ceil(page_size);

    let pages: Vec<PageDescriptor> = (0..total_pages)
        .map(|page_idx| {
            let offset = page_idx * page_size;
            let end = (offset + page_size).min(total);
            let page_mrs = &mrs[offset..end];

            let mut states: BTreeMap<&str, usize> = BTreeMap::new();
            for mr in page_mrs {
                *states.entry(mr.state.as_str()).or_default() += 1;
            }
            let state_parts: Vec<String> =
                states.iter().map(|(s, c)| format!("{} {}", c, s)).collect();
            let summary = format!("MRs #{}-{} ({})", offset + 1, end, state_parts.join(", "));

            PageDescriptor {
                page: page_idx + 1,
                summary,
                item_count: page_mrs.len(),
                offset,
            }
        })
        .collect();

    PageIndex {
        total_items: total,
        shown_items: included_count,
        total_pages,
        pages,
        data_type: "merge_requests".to_string(),
    }
}

/// Build page index for diffs — grouped by directory.
pub fn build_diffs_index(diffs: &[FileDiff], included_count: usize) -> PageIndex {
    let total = diffs.len();
    let page_size = compute_page_size(total, included_count);
    let total_pages = total.div_ceil(page_size);

    let pages: Vec<PageDescriptor> = (0..total_pages)
        .map(|page_idx| {
            let offset = page_idx * page_size;
            let end = (offset + page_size).min(total);
            let page_diffs = &diffs[offset..end];

            // Group by top-level directory for summary
            let mut dirs: BTreeMap<String, usize> = BTreeMap::new();
            let mut total_additions: u32 = 0;
            let mut total_deletions: u32 = 0;
            for d in page_diffs {
                let dir = extract_top_dir(&d.file_path);
                *dirs.entry(dir).or_default() += 1;
                total_additions += d.additions.unwrap_or(0);
                total_deletions += d.deletions.unwrap_or(0);
            }

            let dir_parts: Vec<String> = dirs
                .iter()
                .map(|(d, c)| {
                    if *c == 1 {
                        format!("{d}/*")
                    } else {
                        format!("{d}/* ({c} files)")
                    }
                })
                .collect();

            let summary = format!(
                "{} — +{}/-{}",
                dir_parts.join(", "),
                total_additions,
                total_deletions
            );

            PageDescriptor {
                page: page_idx + 1,
                summary,
                item_count: page_diffs.len(),
                offset,
            }
        })
        .collect();

    PageIndex {
        total_items: total,
        shown_items: included_count,
        total_pages,
        pages,
        data_type: "diffs".to_string(),
    }
}

/// Build page index for discussions — grouped by resolved status.
pub fn build_discussions_index(discussions: &[Discussion], included_count: usize) -> PageIndex {
    let total = discussions.len();
    let page_size = compute_page_size(total, included_count);
    let total_pages = total.div_ceil(page_size);

    let pages: Vec<PageDescriptor> = (0..total_pages)
        .map(|page_idx| {
            let offset = page_idx * page_size;
            let end = (offset + page_size).min(total);
            let page_disc = &discussions[offset..end];

            let resolved = page_disc.iter().filter(|d| d.resolved).count();
            let unresolved = page_disc.len() - resolved;

            let summary = format!(
                "{} discussions ({} unresolved, {} resolved)",
                page_disc.len(),
                unresolved,
                resolved
            );

            PageDescriptor {
                page: page_idx + 1,
                summary,
                item_count: page_disc.len(),
                offset,
            }
        })
        .collect();

    PageIndex {
        total_items: total,
        shown_items: included_count,
        total_pages,
        pages,
        data_type: "discussions".to_string(),
    }
}

/// Build page index for comments — chronological.
pub fn build_comments_index(comments: &[Comment], included_count: usize) -> PageIndex {
    let total = comments.len();
    let page_size = compute_page_size(total, included_count);
    let total_pages = total.div_ceil(page_size);

    let pages: Vec<PageDescriptor> = (0..total_pages)
        .map(|page_idx| {
            let offset = page_idx * page_size;
            let end = (offset + page_size).min(total);
            let page_comments = &comments[offset..end];

            let summary = format!("comments {}-{}", offset + 1, end);

            PageDescriptor {
                page: page_idx + 1,
                summary,
                item_count: page_comments.len(),
                offset,
            }
        })
        .collect();

    PageIndex {
        total_items: total,
        shown_items: included_count,
        total_pages,
        pages,
        data_type: "comments".to_string(),
    }
}

// =============================================================================
// Helpers
// =============================================================================

/// Extract top-level directory from a file path (first 3 segments).
/// "src/app/modules/mcp/tools/foo.ts" → "src/app/modules"
fn extract_top_dir(path: &str) -> String {
    let parts: Vec<&str> = path.split('/').collect();
    if parts.len() <= 2 {
        // Short path: return parent dir or file itself
        if parts.len() == 2 {
            parts[0].to_string()
        } else {
            ".".to_string()
        }
    } else {
        // Take first 3 levels for meaningful grouping
        let depth = 3.min(parts.len() - 1);
        parts[..depth].join("/")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_top_dir() {
        assert_eq!(
            extract_top_dir("src/app/modules/mcp/tools/foo.ts"),
            "src/app/modules"
        );
        assert_eq!(extract_top_dir("README.md"), ".");
        assert_eq!(extract_top_dir("src/main.rs"), "src");
        assert_eq!(extract_top_dir("a/b/c/d/e.rs"), "a/b/c");
    }

    #[test]
    fn test_page_index_toon_output() {
        let index = PageIndex {
            total_items: 52,
            shown_items: 15,
            total_pages: 4,
            pages: vec![
                PageDescriptor {
                    page: 1,
                    summary: "src/app/modules/* (8 files) — +120/-45".to_string(),
                    item_count: 15,
                    offset: 0,
                },
                PageDescriptor {
                    page: 2,
                    summary: "apps/dev-boy-e2e/* (17 files) — +340/-12".to_string(),
                    item_count: 15,
                    offset: 15,
                },
            ],
            data_type: "diffs".to_string(),
        };

        let toon = index.to_toon();
        assert!(toon.contains("[chunks] 15/52 diffs in 4 chunks:"));
        assert!(toon.contains("chunk 1 (offset=0, limit=15):"));
        assert!(toon.contains("<< included above"));
        assert!(toon.contains("chunk 2 (offset=15, limit=15):"));
        assert!(toon.contains("[/chunks]"));
        assert!(toon.contains("You may not need all chunks"));
        // Only chunk 1 is marked as included
        let lines: Vec<&str> = toon
            .lines()
            .filter(|l| l.contains("included above"))
            .collect();
        assert_eq!(lines.len(), 1, "Only chunk 1 should be marked as included");
    }

    #[test]
    fn test_build_diffs_index() {
        let diffs: Vec<FileDiff> = (0..10)
            .map(|i| FileDiff {
                file_path: format!("src/app/file_{}.ts", i),
                diff: format!("diff content {}", i),
                additions: Some(10),
                deletions: Some(5),
                ..Default::default()
            })
            .collect();

        let index = build_diffs_index(&diffs, 5);
        assert_eq!(index.total_items, 10);
        assert_eq!(index.total_pages, 2);
        assert_eq!(index.pages[0].item_count, 5);
        assert_eq!(index.pages[0].offset, 0);
        assert_eq!(index.pages[1].offset, 5);
    }
}
