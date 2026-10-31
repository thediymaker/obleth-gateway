//! Shared configuration and domain types for the obleth gateway.
//!
//! This crate has no internal dependencies so every other crate can rely on a
//! single canonical definition of tenants, keys, quotas and runtime config.

pub mod budget;
pub mod capacity;
pub mod config;
pub mod failure;
pub mod jwt;
pub mod keys;
pub mod manifest;
pub mod reasoning;
pub mod routing;
pub mod types;

pub use config::{CapacityDiscoveryConfig, Config, SessionAffinityConfig, SlackAlertConfig};
pub use failure::{
    class_for_status, clean_reason, ledger_failure, reason_for_status, reason_from_body,
    ErrorClass, Failure, EFFECTIVE_CLASS_SQL, ERROR_REASON_MAX_CHARS, PLATFORM_CLASSES_SQL,
};
pub use jwt::{
    identity_key_hash, jwt_issuers_from_env, parse_jwt_issuers, sanitize_device_id, JwtConfigError,
    JwtIssuerConfig, ALLOWED_JWT_ALGORITHMS, DEVICE_ID_MAX_LEN,
};
pub use keys::{
    cache_key, content_hash, end_user_key_id, generate_api_key, hash_api_key, normalize_end_user,
    pepper_is_set, GeneratedKey, END_USER_MAX_CHARS,
};
pub use manifest::{
    endpoint_to_manifest_entry, model_to_manifest_entry, resolve_endpoint, resolve_model,
    EndpointConfig, ManifestEndpoint, ManifestError, ManifestModel, ModelConfig, ModelImportEntry,
    ModelImportReport, ModelManifest, ResolvedManifestEndpoint, ResolvedManifestModel,
    IMPORT_ACTION_CREATED, IMPORT_ACTION_UNCHANGED, IMPORT_ACTION_UPDATED, MODEL_MANIFEST_FORMAT,
    MODEL_MANIFEST_VERSION,
};
pub use reasoning::{
    FamilyMembers, ModelFamily, ModelReasoning, ReasoningAction, ReasoningLevel, ReasoningMode,
    ReasoningPlan,
};
pub use types::*;
