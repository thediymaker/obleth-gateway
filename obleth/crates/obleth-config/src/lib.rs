//! Shared configuration and domain types for the obleth gateway.
//!
//! This crate has no internal dependencies so every other crate can rely on a
//! single canonical definition of tenants, keys, quotas and runtime config.

pub mod config;
pub mod jwt;
pub mod keys;
pub mod manifest;
pub mod routing;
pub mod types;

pub use config::{Config, SlackAlertConfig};
pub use jwt::{
    identity_key_hash, jwt_issuers_from_env, parse_jwt_issuers, sanitize_device_id, JwtConfigError,
    JwtIssuerConfig, ALLOWED_JWT_ALGORITHMS, DEVICE_ID_MAX_LEN,
};
pub use keys::{
    cache_key, content_hash, generate_api_key, hash_api_key, pepper_is_set, GeneratedKey,
};
pub use manifest::{
    endpoint_to_manifest_entry, model_to_manifest_entry, resolve_endpoint, resolve_model,
    EndpointConfig, ManifestEndpoint, ManifestError, ManifestModel, ModelConfig, ModelImportEntry,
    ModelImportReport, ModelManifest, ResolvedManifestEndpoint, ResolvedManifestModel,
    IMPORT_ACTION_CREATED, IMPORT_ACTION_UNCHANGED, IMPORT_ACTION_UPDATED, MODEL_MANIFEST_FORMAT,
    MODEL_MANIFEST_VERSION,
};
pub use types::*;
