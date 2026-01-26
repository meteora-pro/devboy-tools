//! Core traits, types, and error handling for devboy-tools.
//!
//! This crate provides the foundational abstractions used across all devboy components.

pub mod error;
pub mod provider;
pub mod types;

pub use error::{Error, Result};
pub use provider::Provider;
