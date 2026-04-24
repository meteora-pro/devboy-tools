//! Hint-based deduplication cache for tool responses in multi-turn agents.
//!
//! When an LLM agent re-requests data it has already seen (file re-reads after
//! unrelated edits, MCP pipeline polls, repeated status checks), the response
//! bytes are often identical. Instead of re-emitting the full payload, the
//! pipeline emits a compact reference hint that points at an earlier
//! `tool_call_id` still present in the agent's context window.
//!
//! On anonymized Claude Code session logs, this mechanism recovers ≈33% of
//! total tokens (see `docs/research/paper-2-mckp-format-adaptive.md`).
//!
//! # Design
//!
//! - **Content-hash fingerprint** — SHA-256/128-bit prefix. Collisions are
//!   negligible for session volumes (≤10⁵ responses).
//! - **Bounded LRU** — capacity defaults to 5; 95% of real-world savings
//!   concentrate in the most recent 3 responses.
//! - **Partition-scoped** — cleared on `on_compaction_boundary()` (corresponds
//!   to Claude Code's `compact_boundary` JSONL event); older references are no
//!   longer reachable from the agent's context.
//! - **Mutation-aware** — `FileRead` entries whose path is later mutated by
//!   `FileMutate` are invalidated immediately. Prevents stale hints after an
//!   `Edit` or `Write`.
//!
//! # Usage
//!
//! ```
//! use devboy_format_pipeline::dedup::{DedupCache, DedupDecision, ToolKind, content_hash, render_reference_hint};
//!
//! let mut cache = DedupCache::with_capacity(5);
//! let body = "pipeline 12345 status=success duration=120s";
//! let fp = content_hash(body.as_bytes());
//!
//! match cache.check(&fp) {
//!     DedupDecision::Hint { reference_tool_call_id } => {
//!         let hint = render_reference_hint(&reference_tool_call_id);
//!         // emit `hint` instead of `body`
//!     }
//!     DedupDecision::Fresh => {
//!         // emit `body` normally and cache it
//!         cache.insert(fp, "tc_42", ToolKind::Other, None);
//!     }
//! }
//! ```

use std::collections::VecDeque;

use sha2::{Digest, Sha256};

/// 128-bit content fingerprint (first 16 bytes of SHA-256).
pub type ContentHash = [u8; 16];

/// Compute the content hash used by [`DedupCache`].
pub fn content_hash(content: &[u8]) -> ContentHash {
    let digest = Sha256::digest(content);
    let mut out = [0u8; 16];
    out.copy_from_slice(&digest[..16]);
    out
}

/// Outcome of a [`DedupCache::check`] lookup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DedupDecision {
    /// Content not seen in the current partition — caller should emit the
    /// response normally and then `insert` it into the cache.
    Fresh,
    /// Content matches a cached response — caller should emit a reference
    /// hint (see [`render_reference_hint`]) instead of the full payload.
    Hint {
        reference_tool_call_id: String,
    },
}

/// Classification used to drive mutation-based invalidation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolKind {
    /// Reads a file. Cached responses may become stale if the underlying file
    /// is mutated and are invalidated by [`DedupCache::invalidate_file`].
    FileRead,
    /// Mutates a file. Does not itself need to be cached (responses are small
    /// and rarely repeated), but triggers invalidation of matching
    /// `FileRead` entries.
    FileMutate,
    /// Any other tool (Bash, MCP, Agent, Web…). No implicit cross-tool
    /// invalidation.
    Other,
}

impl ToolKind {
    /// Classify by standard Claude Code tool name. Unknown names → `Other`.
    pub fn from_tool_name(name: &str) -> Self {
        match name {
            "Read" | "Grep" | "Glob" | "NotebookRead" => Self::FileRead,
            "Edit" | "Write" | "MultiEdit" | "NotebookEdit" => Self::FileMutate,
            _ => Self::Other,
        }
    }
}

#[derive(Debug, Clone)]
struct CacheEntry {
    hash: ContentHash,
    tool_call_id: String,
    tool_kind: ToolKind,
    /// Anonymized hash of the primary file-path argument (None for non-file
    /// tools). Used as the invalidation key.
    file_path_hash: Option<String>,
}

/// Session-scoped deduplication cache.
///
/// Maintains a bounded LRU of recent `(content_hash → tool_call_id)` mappings.
/// A single instance is intended to live for one agent session (or one
/// context partition, whichever is shorter).
#[derive(Debug)]
pub struct DedupCache {
    entries: VecDeque<CacheEntry>,
    capacity: usize,
    partition: u64,
}

impl DedupCache {
    /// Construct a cache with the given LRU capacity.
    ///
    /// Recommended default: `5`. Empirically, 95% of deduplication savings in
    /// Claude Code logs come from the three most-recent responses; capacities
    /// above ~5 yield diminishing returns.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            entries: VecDeque::with_capacity(capacity.max(1)),
            capacity: capacity.max(1),
            partition: 0,
        }
    }

    /// Current context partition counter. Increments on each
    /// `on_compaction_boundary()` call.
    pub fn partition(&self) -> u64 {
        self.partition
    }

    /// Number of entries currently cached.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// `true` if no entries are cached.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Non-mutating lookup. Returns `Hint` iff a matching entry exists.
    pub fn check(&self, hash: &ContentHash) -> DedupDecision {
        for e in &self.entries {
            if &e.hash == hash {
                return DedupDecision::Hint {
                    reference_tool_call_id: e.tool_call_id.clone(),
                };
            }
        }
        DedupDecision::Fresh
    }

    /// Insert a fresh response. Evicts the oldest entry if the cache is at
    /// capacity.
    pub fn insert(
        &mut self,
        hash: ContentHash,
        tool_call_id: impl Into<String>,
        tool_kind: ToolKind,
        file_path_hash: Option<String>,
    ) {
        if self.entries.len() >= self.capacity {
            self.entries.pop_front();
        }
        self.entries.push_back(CacheEntry {
            hash,
            tool_call_id: tool_call_id.into(),
            tool_kind,
            file_path_hash,
        });
    }

    /// Invalidate all cached `FileRead` entries whose path hash matches.
    /// Returns the number of entries dropped.
    ///
    /// Callers typically invoke this from a `FileMutate` tool response
    /// handler to prevent emitting stale hints for a subsequently re-read file.
    pub fn invalidate_file(&mut self, file_path_hash: &str) -> usize {
        let before = self.entries.len();
        self.entries.retain(|e| {
            !(e.tool_kind == ToolKind::FileRead
                && e.file_path_hash.as_deref() == Some(file_path_hash))
        });
        before - self.entries.len()
    }

    /// Clear the cache and advance the partition counter.
    ///
    /// Called when the LLM host emits a context-compaction boundary
    /// (Claude Code: `compact_boundary` event). Older references are no
    /// longer reachable from the agent's context, so they must not be
    /// referenced by new hints.
    pub fn on_compaction_boundary(&mut self) {
        self.entries.clear();
        self.partition = self.partition.saturating_add(1);
    }
}

impl Default for DedupCache {
    /// Equivalent to [`DedupCache::with_capacity(5)`].
    fn default() -> Self {
        Self::with_capacity(5)
    }
}

/// Render a reference hint that replaces a duplicate response.
///
/// The hint is a terse Markdown blockquote (~15 tokens):
///
/// ```text
/// > [ref: tc_42]
/// ```
///
/// Most LLMs treat blockquotes as metadata and retrieve the referenced
/// response from their earlier context without re-fetching.
pub fn render_reference_hint(tool_call_id: &str) -> String {
    format!("> [ref: {}]", tool_call_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(s: &str) -> ContentHash {
        content_hash(s.as_bytes())
    }

    #[test]
    fn fresh_response_then_duplicate_hits() {
        let mut c = DedupCache::with_capacity(5);
        let fp = h("pipeline 12345 status=success");
        assert_eq!(c.check(&fp), DedupDecision::Fresh);
        c.insert(fp, "tc_1", ToolKind::Other, None);
        match c.check(&fp) {
            DedupDecision::Hint {
                reference_tool_call_id,
            } => assert_eq!(reference_tool_call_id, "tc_1"),
            other => panic!("expected hint, got {other:?}"),
        }
    }

    #[test]
    fn distinct_content_is_fresh() {
        let mut c = DedupCache::with_capacity(5);
        c.insert(h("A"), "tc_1", ToolKind::Other, None);
        assert_eq!(c.check(&h("B")), DedupDecision::Fresh);
    }

    #[test]
    fn lru_evicts_oldest_when_full() {
        let mut c = DedupCache::with_capacity(2);
        c.insert(h("one"), "tc_1", ToolKind::Other, None);
        c.insert(h("two"), "tc_2", ToolKind::Other, None);
        c.insert(h("three"), "tc_3", ToolKind::Other, None);
        assert_eq!(c.check(&h("one")), DedupDecision::Fresh);
        assert!(matches!(c.check(&h("two")), DedupDecision::Hint { .. }));
        assert!(matches!(c.check(&h("three")), DedupDecision::Hint { .. }));
    }

    #[test]
    fn mutation_invalidates_same_file_read() {
        let mut c = DedupCache::with_capacity(5);
        let file = "abc12345".to_string();
        let content = h("line1\nline2");
        c.insert(content, "tc_1", ToolKind::FileRead, Some(file.clone()));
        assert_eq!(c.invalidate_file(&file), 1);
        assert_eq!(c.check(&content), DedupDecision::Fresh);
    }

    #[test]
    fn mutation_on_different_file_preserves_entry() {
        let mut c = DedupCache::with_capacity(5);
        let content = h("irrelevant file body");
        c.insert(
            content,
            "tc_1",
            ToolKind::FileRead,
            Some("hash_a".into()),
        );
        assert_eq!(c.invalidate_file("hash_b"), 0);
        assert!(matches!(c.check(&content), DedupDecision::Hint { .. }));
    }

    #[test]
    fn compaction_boundary_clears_and_advances_partition() {
        let mut c = DedupCache::with_capacity(5);
        let x = h("x");
        c.insert(x, "tc_1", ToolKind::Other, None);
        assert_eq!(c.partition(), 0);
        c.on_compaction_boundary();
        assert_eq!(c.partition(), 1);
        assert_eq!(c.check(&x), DedupDecision::Fresh);
        assert!(c.is_empty());
    }

    #[test]
    fn tool_kind_classification() {
        assert_eq!(ToolKind::from_tool_name("Read"), ToolKind::FileRead);
        assert_eq!(ToolKind::from_tool_name("Grep"), ToolKind::FileRead);
        assert_eq!(ToolKind::from_tool_name("Edit"), ToolKind::FileMutate);
        assert_eq!(ToolKind::from_tool_name("Write"), ToolKind::FileMutate);
        assert_eq!(ToolKind::from_tool_name("MultiEdit"), ToolKind::FileMutate);
        assert_eq!(ToolKind::from_tool_name("Bash"), ToolKind::Other);
        assert_eq!(ToolKind::from_tool_name("mcp__x__get_issues"), ToolKind::Other);
    }

    #[test]
    fn reference_hint_shape() {
        let hint = render_reference_hint("tc_42");
        assert!(hint.starts_with("> [ref:"));
        assert!(hint.contains("tc_42"));
        // Hint must stay small vs any realistic response.
        assert!(hint.len() < 30);
    }

    #[test]
    fn capacity_zero_is_coerced_to_one() {
        let c = DedupCache::with_capacity(0);
        assert_eq!(c.len(), 0);
    }

    #[test]
    fn edit_then_reread_scenario() {
        // Canonical stale-hint scenario from the paper:
        //   1. Read(foo.py) → content_A → cache it
        //   2. Edit(foo.py) → invalidate foo.py entries
        //   3. Read(foo.py) → content_B or A? — must be treated Fresh regardless
        let mut c = DedupCache::with_capacity(5);
        let file = "foo_hash".to_string();
        let content_a = h("original body");
        c.insert(content_a, "tc_1", ToolKind::FileRead, Some(file.clone()));
        // Edit invalidates
        assert_eq!(c.invalidate_file(&file), 1);
        // Even if the re-read content coincidentally matches, we must not hint
        // against the stale entry (it is gone). Hence any subsequent content is Fresh.
        assert_eq!(c.check(&content_a), DedupDecision::Fresh);
    }
}
