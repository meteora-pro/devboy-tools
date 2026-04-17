//! Asset management for devboy-tools.
//!
//! This crate provides a local, on-disk cache for files downloaded from
//! providers (issue attachments, MR uploads, messenger files, ...) together
//! with an index of metadata, LRU rotation, and an integrity check.
//!
//! The public surface is centered on [`AssetManager`], which wraps a
//! [`CacheManager`] and an on-disk [`AssetIndex`].
//!
//! See `docs/architecture/adr/ADR-010-asset-management.md` for the full design.

#![deny(missing_docs)]

pub mod cache;
pub mod config;
pub mod error;
pub mod index;
pub mod manager;
pub mod rotation;

pub use cache::{CacheManager, StoredFile};
pub use config::{AssetConfig, EvictionPolicy, ResolvedAssetConfig};
pub use error::{AssetError, Result};
pub use index::{AssetIndex, CachedAsset, NewCachedAsset};
pub use manager::{AssetManager, ResolvedAsset, StoreRequest};
pub use rotation::{RotationStats, Rotator};
