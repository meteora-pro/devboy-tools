//! Common types used across providers.

use serde::{Deserialize, Serialize};

/// Represents a user from a git hosting service.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    pub id: u64,
    pub username: String,
    pub name: Option<String>,
    pub email: Option<String>,
    pub avatar_url: Option<String>,
}

/// Represents an issue from a git hosting service.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Issue {
    pub id: u64,
    pub iid: u64,
    pub title: String,
    pub description: Option<String>,
    pub state: String,
    pub author: Option<User>,
    pub assignees: Vec<User>,
    pub labels: Vec<String>,
    pub created_at: String,
    pub updated_at: String,
    pub web_url: String,
}

/// Represents a merge request / pull request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MergeRequest {
    pub id: u64,
    pub iid: u64,
    pub title: String,
    pub description: Option<String>,
    pub state: String,
    pub source_branch: String,
    pub target_branch: String,
    pub author: Option<User>,
    pub assignees: Vec<User>,
    pub reviewers: Vec<User>,
    pub labels: Vec<String>,
    pub draft: bool,
    pub created_at: String,
    pub updated_at: String,
    pub web_url: String,
}

/// Represents a comment on an issue or merge request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Comment {
    pub id: u64,
    pub body: String,
    pub author: Option<User>,
    pub created_at: String,
    pub updated_at: String,
}
