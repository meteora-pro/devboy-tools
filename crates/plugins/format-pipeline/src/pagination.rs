//! Cursor-based pagination for budget-trimmed responses.
//!
//! Since the MCP spec (2025-06-18) does not support pagination for tool responses,
//! we implement pagination at the middleware level through a self-modifying tool interface.
//!
//! The `_page_cursor` parameter is added transparently by the pipeline enricher.
//! The cursor encodes position in the overflow set so subsequent requests
//! can retrieve remaining data.

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};

/// Cursor for navigating paginated overflow data.
///
/// Encoded as base64(JSON) for compactness in tool parameters.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PageCursor {
    /// Cursor format version (for forward compatibility).
    pub v: u8,
    /// Data type (e.g. "issues", "diffs", "discussions").
    pub data_type: String,
    /// Offset into the full dataset.
    pub offset: usize,
    /// Total number of items in the full dataset.
    pub total: usize,
    /// Strategy used for trimming (for consistent pagination).
    pub strategy: String,
    /// Budget tokens used.
    pub budget: usize,
}

impl PageCursor {
    /// Create a new cursor for the first overflow page.
    pub fn new(
        data_type: impl Into<String>,
        offset: usize,
        total: usize,
        strategy: impl Into<String>,
        budget: usize,
    ) -> Self {
        Self {
            v: 1,
            data_type: data_type.into(),
            offset,
            total,
            strategy: strategy.into(),
            budget,
        }
    }

    /// Encode cursor to a URL-safe base64 string.
    pub fn encode(&self) -> String {
        let json = serde_json::to_string(self).expect("PageCursor serialization should not fail");
        URL_SAFE_NO_PAD.encode(json.as_bytes())
    }

    /// Decode cursor from a base64 string.
    pub fn decode(encoded: &str) -> Result<Self, PaginationError> {
        let bytes = URL_SAFE_NO_PAD
            .decode(encoded)
            .map_err(|e| PaginationError::InvalidCursor(format!("base64 decode: {e}")))?;

        let json = String::from_utf8(bytes)
            .map_err(|e| PaginationError::InvalidCursor(format!("UTF-8 decode: {e}")))?;

        serde_json::from_str(&json)
            .map_err(|e| PaginationError::InvalidCursor(format!("JSON parse: {e}")))
    }

    /// Whether there are more pages after this one.
    pub fn has_more(&self) -> bool {
        self.offset < self.total
    }

    /// Number of remaining items.
    pub fn remaining(&self) -> usize {
        self.total.saturating_sub(self.offset)
    }

    /// Create cursor for the next page.
    pub fn next_page(&self, items_in_current_page: usize) -> Option<Self> {
        let next_offset = self.offset + items_in_current_page;
        if next_offset >= self.total {
            return None;
        }
        Some(Self {
            v: self.v,
            data_type: self.data_type.clone(),
            offset: next_offset,
            total: self.total,
            strategy: self.strategy.clone(),
            budget: self.budget,
        })
    }
}

/// Pagination-specific errors.
#[derive(Debug, thiserror::Error)]
pub enum PaginationError {
    #[error("Invalid cursor: {0}")]
    InvalidCursor(String),
}

/// Generate a pagination hint for the agent to include in the response.
pub fn create_pagination_hint(cursor: &PageCursor, items_shown: usize) -> String {
    let remaining = cursor.remaining();
    format!(
        "Showing {}/{} {}. {} more available. Use `_page_cursor: \"{}\"` to get the next page.",
        items_shown,
        cursor.total,
        cursor.data_type,
        remaining,
        cursor.encode()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cursor_encode_decode_roundtrip() {
        let cursor = PageCursor::new("issues", 20, 50, "element_count", 8000);
        let encoded = cursor.encode();
        let decoded = PageCursor::decode(&encoded).unwrap();
        assert_eq!(cursor, decoded);
    }

    #[test]
    fn test_cursor_fields() {
        let cursor = PageCursor::new("diffs", 10, 30, "size_proportional", 4000);
        assert_eq!(cursor.v, 1);
        assert_eq!(cursor.data_type, "diffs");
        assert_eq!(cursor.offset, 10);
        assert_eq!(cursor.total, 30);
        assert_eq!(cursor.strategy, "size_proportional");
        assert_eq!(cursor.budget, 4000);
    }

    #[test]
    fn test_cursor_has_more() {
        let cursor = PageCursor::new("issues", 20, 50, "element_count", 8000);
        assert!(cursor.has_more());

        let cursor = PageCursor::new("issues", 50, 50, "element_count", 8000);
        assert!(!cursor.has_more());
    }

    #[test]
    fn test_cursor_remaining() {
        let cursor = PageCursor::new("issues", 20, 50, "element_count", 8000);
        assert_eq!(cursor.remaining(), 30);

        let cursor = PageCursor::new("issues", 50, 50, "element_count", 8000);
        assert_eq!(cursor.remaining(), 0);
    }

    #[test]
    fn test_cursor_next_page() {
        let cursor = PageCursor::new("issues", 0, 50, "element_count", 8000);
        let next = cursor.next_page(20).unwrap();
        assert_eq!(next.offset, 20);
        assert_eq!(next.total, 50);

        let next2 = next.next_page(20).unwrap();
        assert_eq!(next2.offset, 40);

        let next3 = next2.next_page(20);
        assert!(next3.is_none(), "Should be None when offset >= total");
    }

    #[test]
    fn test_cursor_next_page_exact_boundary() {
        let cursor = PageCursor::new("issues", 30, 50, "element_count", 8000);
        let next = cursor.next_page(20);
        assert!(next.is_none(), "30 + 20 = 50 = total, no more pages");
    }

    #[test]
    fn test_decode_invalid_base64() {
        let result = PageCursor::decode("not-valid-base64!!!");
        assert!(result.is_err());
    }

    #[test]
    fn test_decode_invalid_json() {
        let encoded = URL_SAFE_NO_PAD.encode(b"not json");
        let result = PageCursor::decode(&encoded);
        assert!(result.is_err());
    }

    #[test]
    fn test_create_pagination_hint() {
        let cursor = PageCursor::new("issues", 20, 50, "element_count", 8000);
        let hint = create_pagination_hint(&cursor, 20);
        assert!(hint.contains("20/50"));
        assert!(hint.contains("30 more"));
        assert!(hint.contains("_page_cursor"));
    }

    #[test]
    fn test_cursor_encode_is_compact() {
        let cursor = PageCursor::new("issues", 0, 100, "element_count", 8000);
        let encoded = cursor.encode();
        // Should be reasonably compact (< 200 chars)
        assert!(
            encoded.len() < 200,
            "Encoded cursor should be compact, got {} chars",
            encoded.len()
        );
    }

    #[test]
    fn test_multi_page_simulation() {
        let total = 50;
        let page_size = 15;
        let mut cursor = PageCursor::new("issues", 0, total, "element_count", 8000);
        let mut pages = 0;
        let mut total_items = 0;

        loop {
            let items_this_page = page_size.min(cursor.remaining());
            total_items += items_this_page;
            pages += 1;

            match cursor.next_page(items_this_page) {
                Some(next) => cursor = next,
                None => break,
            }
        }

        assert_eq!(pages, 4); // 15+15+15+5
        assert_eq!(total_items, 50);
    }
}
