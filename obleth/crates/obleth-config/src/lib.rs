//! Shared configuration and domain types for the obleth gateway.
//!
//! This crate has no internal dependencies so every other crate can rely on a
//! single canonical definition of tenants, keys, quotas and runtime config.

pub mod config;
pub mod keys;
pub mod types;

pub use config::{Config, SlackAlertConfig};
pub use keys::{cache_key, content_hash, generate_api_key, hash_api_key, pepper_is_set, GeneratedKey};
pub use types::*;
