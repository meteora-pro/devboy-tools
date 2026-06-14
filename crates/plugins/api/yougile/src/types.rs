//! Shared YouGile wire and helper types.

use serde::{Deserialize, Serialize};

/// Minimal board descriptor reused by follow-up provider implementation work.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct YouGileBoardRef {
    pub id: String,
    pub title: String,
}

/// Minimal column descriptor reused by follow-up provider implementation work.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct YouGileColumnRef {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub board_id: Option<String>,
}
