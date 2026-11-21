//! OpenAPI document for the Management API (served at `/api/v1/openapi.json`).
//!
//! Every route in `router()` needs a `#[utoipa::path(...)]` with one declared
//! tag, an entry in `paths(...)`, and its types in `components(schemas(...))`;
//! the tests below fail when one is missing.

use utoipa::OpenApi;

use crate::autotune::{AutotuneReport, AutotuneRequest, AutotuneStep, KneeReason, WorkloadProfile};
use crate::model_health::{
    BulkModelHealthResult, ModelActivation, UpdateModelHealthConfig, ValidateModelRequest,
    ValidateModelResult,
};
use crate::usage::{
    CacheStats, CostAgg, KeyRejectionAgg, KeySource, KeyUsageSummary, KeyUsageSummaryQuery,
    KeyWatchQuery, ModelSpeedPoint, ModelUsageTimePoint, TenantUsageTimePoint, UsageAgg,
    UsageBreakdownQuery, UsageDailyQuery, UsageDailyRow, UsageFailureGroup, UsageFailureGroups,
    UsageKeyAgg, UsageKeyModelBreakdown, UsageLogBucket, UsageLogFacet, UsageLogFacets,
    UsageLogFailure, UsageLogHistogram, UsageLogQuery, UsageLogRow, UsageModelAgg, UsageQuery,
    UsageSeriesQuery, UsageSpeedQuery, UsageTimePoint,
};
use crate::{
    AlertSettingsView, ApplyAutotuneCapacity, AuditQuery, AutoRouterSettingsView, BoonSettingsView,
    CapacityDiscoveryModelView, CapacityDiscoveryView, CapacityView, CharoSettingsView,
    CompactUsageResult, CompressorStatusView, CreateFairshareGroup, CreateKey, CreateMcpServer,
    CreateModel, CreateModelEndpoint, CreateTenant, CreatedKey, EmailSettingsView,
    EnergySettingsView, EnergyTestResult, FairshareHistoryPointView, FairshareHistoryQuery,
    FairshareHistoryView, FairshareLiveView, GroupFairshareView, KeyFairshareView, KeyPrefixView,
    ListKeysQuery, LiveStats, McpServerView, ModelEndpointView, ModelPoolView, ModelPowerMatch,
    ModelPowerTest, ModelRouteView, ModelVariantWrite, OverviewSummaryView, SetCapacity,
    SetDisabled, SetModelCache, SetModelCapacity, SetModelCapacityMode, SetModelReliability,
    SetModelStatus, SetModelWeight, SetTenantAllowlist, SetTenantBudget, SetTenantCompression,
    SetTenantGuardrails, SetTenantSchedule, SetTenantStatus, SetTenantSynthetic,
    TenantFairshareView, TestAlertResult, TestEnergyQuery, UpdateAlertSettings,
    UpdateAutoRouterSettings, UpdateBoonSettings, UpdateEmailSettings, UpdateEnergySettings,
    UpdateGroupWeight, UpdateKey, UpdateKeyPrefix, UpdateMcpServer, UpdateModel,
    UpdateModelEndpoint, UpdateQuota, UpdateTenant, UpdateTenantGroup, UpdateUsageRetention,
    UpdateWeight, UsageRetentionView,
};
use obleth_config::{
    ApiKey, ApiKeyBackup, AppSettingBackup, BackupData, BackupEncryption, CompressionPolicy,
    ConfigBackup, FairshareGroup, FairshareGroupBackup, GuardrailsPolicy, ManagedModelSpec,
    ManifestEndpoint, ManifestModel, McpServerBackup, ModelBackup, ModelEndpointBackup,
    ModelHealthCheck, ModelHealthDetail, ModelHealthSummary, ModelImportEntry, ModelImportReport,
    ModelManifest, ModelReplica, ModelVariant, RestoreCounts, RestoreReport, Tenant, TenantBackup,
    WeeklyWindow,
};

#[derive(OpenApi)]
#[openapi(
    info(
        title = "obleth Management API",
        description = "Programmatic control plane: tenants, keys, models, usage, fairshare, and settings. Update bodies follow one rule: an omitted field keeps its stored value, and null clears a field that can be empty. A write to one setting of a resource (`/tenants/{id}/budget`, `/keys/{id}/disabled`, `/models/{id}/status`) is a PUT and answers 200 with the updated resource. A DELETE answers 204, or 404 when there is nothing by that id. The tenant and group setters also accept PATCH, their verb up to v2.13.0, for one release."
    ),
    paths(
        // meta
        crate::get_version,
        crate::resync_resolver_cache,
        // tenants
        crate::create_tenant,
        crate::list_tenants,
        crate::get_tenant,
        crate::update_tenant,
        crate::delete_tenant,
        crate::set_tenant_status,
        crate::set_tenant_schedule,
        crate::set_tenant_budget,
        crate::set_tenant_allowlist,
        crate::set_tenant_guardrails,
        crate::set_tenant_compression,
        crate::set_tenant_synthetic_handler,
        crate::set_tenant_tracing_handler,
        crate::set_tenant_weight,
        crate::set_tenant_quota,
        crate::set_tenant_group,
        // keys
        crate::create_key,
        crate::list_keys,
        crate::keys_page::count_keys,
        crate::keys_page::key_stats,
        crate::get_key,
        crate::update_key,
        crate::move_key,
        crate::get_budget_usage,
        crate::delete_key,
        crate::set_key_disabled,
        crate::set_key_tracing_handler,
        crate::set_key_end_user_fairshare_handler,
        crate::set_key_background_handler,
        crate::get_key_usage,
        // usage & costs
        crate::get_usage,
        crate::get_usage_keys,
        crate::get_usage_keys_summary,
        crate::get_usage_key_sources,
        crate::get_usage_key_rejections,
        crate::get_usage_models,
        crate::get_usage_series,
        crate::get_usage_series_tenants,
        crate::get_usage_series_models,
        crate::get_usage_speed,
        crate::get_usage_breakdown,
        crate::get_cache_stats,
        crate::get_usage_logs,
        crate::get_usage_log_histogram,
        crate::get_usage_log_facets,
        crate::get_usage_failure_groups,
        crate::get_request_spans,
        crate::get_usage_daily,
        crate::compact_usage,
        crate::get_costs,
        crate::get_stats,
        crate::get_overview,
        crate::get_overview_summary,
        // fairshare
        crate::get_fairshare_live,
        crate::get_capacity_discovery,
        crate::get_capacity_services,
        crate::get_fairshare_history,
        crate::list_fairshare_groups,
        crate::create_fairshare_group,
        crate::set_fairshare_group_weight,
        // models
        crate::create_model,
        crate::list_models,
        crate::get_model,
        crate::update_model,
        crate::delete_model,
        crate::set_model_capacity,
        crate::set_model_status,
        crate::set_model_reasoning,
        crate::set_model_listing,
        crate::set_model_rejected_request_fields,
        crate::set_model_session_affinity,
        crate::list_model_families,
        crate::create_model_family,
        crate::update_model_family,
        crate::delete_model_family,
        crate::set_model_capacity_mode,
        crate::autotune_model,
        crate::apply_autotune_capacity,
        crate::set_model_weight,
        crate::set_model_cache,
        crate::set_model_reliability,
        crate::knowledge::get_model_collections,
        crate::knowledge::set_model_collections,
        crate::list_model_endpoints,
        crate::list_all_endpoints,
        crate::create_model_endpoint,
        crate::update_model_endpoint,
        crate::delete_model_endpoint,
        // managed models & replicas
        crate::list_managed_models,
        crate::get_managed_model,
        crate::put_managed_model,
        crate::delete_managed_model,
        crate::set_provision_error,
        crate::retry_managed_model,
        crate::roll_managed_model,
        crate::set_managed_backoff,
        crate::list_replicas,
        crate::list_all_replicas,
        crate::create_replica,
        crate::patch_replica,
        crate::restart_replica,
        crate::delete_replica,
        crate::clear_lost_replicas,
        crate::list_deployment_launches,
        crate::model_health::list_health,
        crate::model_health::get_health,
        crate::model_health::check_one,
        crate::model_health::activate,
        crate::model_health::check_all,
        crate::model_health::update_config,
        crate::model_health::validate_model,
        // mcp
        crate::create_mcp_server,
        crate::list_mcp_servers,
        crate::get_mcp_server,
        crate::update_mcp_server,
        crate::delete_mcp_server,
        // audit & capacity
        crate::get_audit,
        crate::batches::list_batches,
        crate::batches::get_batch,
        crate::batches::cancel_batch,
        crate::get_daily_stats,
        crate::get_capacity,
        crate::set_capacity,
        crate::clear_capacity,
        // settings
        crate::get_alert_settings,
        crate::put_alert_settings,
        crate::test_alert_settings,
        crate::get_auto_router_settings,
        crate::router_readiness::get_router_readiness,
        crate::advice::get_advice,
        crate::advice::put_snoozed,
        crate::advice::delete_snoozed,
        crate::put_auto_router_settings,
        crate::simulate_route,
        crate::get_boon_settings,
        crate::put_boon_settings,
        crate::knowledge::get_knowledge_settings,
        crate::knowledge::put_knowledge_settings,
        // knowledge base
        crate::knowledge::list_collections,
        crate::knowledge::create_collection,
        crate::knowledge::get_collection,
        crate::knowledge::update_collection,
        crate::knowledge::delete_collection,
        crate::knowledge::list_documents,
        crate::knowledge::upload_document,
        crate::knowledge::delete_document,
        crate::knowledge::reindex_collection,
        crate::knowledge::reindex_document,
        crate::knowledge::search_collection,
        crate::get_energy_settings,
        crate::put_energy_settings,
        crate::test_energy_query,
        crate::get_compressor_status,
        crate::get_charo_settings,
        crate::put_charo_settings,
        crate::get_usage_retention,
        crate::put_usage_retention,
        crate::get_key_prefix,
        crate::put_key_prefix,
        crate::get_network_settings,
        crate::put_network_settings,
        crate::slurm_settings::get_slurm_settings,
        crate::slurm_settings::put_slurm_settings,
        crate::slurm_settings::test_slurm_settings,
        crate::slurm_settings::get_slurm_settings_resolved,
        crate::slurm_resources::get_slurm_resources,
        // recipes
        crate::recipes::list_recipes,
        crate::recipes::create_recipe,
        crate::recipes::update_recipe,
        crate::recipes::delete_recipe,
        // backup
        crate::backup::export_backup,
        crate::backup::restore_backup,
        // model manifest
        crate::models_io::export_models,
        crate::models_io::import_models,
    ),
    components(schemas(
        Tenant,
        ApiKey,
        FairshareGroup,
        ModelRouteView,
        ModelEndpointView,
        McpServerView,
        ModelHealthSummary,
        ModelHealthCheck,
        ModelHealthDetail,
        UpdateModelHealthConfig,
        BulkModelHealthResult,
        ModelActivation,
        ValidateModelRequest,
        ValidateModelResult,
        CreateTenant,
        UpdateTenant,
        SetTenantStatus,
        SetTenantSynthetic,
        SetTenantSchedule,
        SetTenantBudget,
        SetTenantAllowlist,
        SetTenantGuardrails,
        GuardrailsPolicy,
        SetTenantCompression,
        CompressionPolicy,
        WeeklyWindow,
        UpdateWeight,
        crate::MoveKey,
        crate::DailyStatsView,
        crate::DailyStatsItem,
        crate::DailyStat,
        crate::BudgetUsage,
        crate::keys_page::KeyCount,
        crate::keys_page::KeyStatsView,
        crate::keys_page::BusiestKey,
        crate::keys_page::NearBudgetKey,
        crate::keys_page::KeyStatsQuery,
        UpdateQuota,
        UpdateTenantGroup,
        CreateFairshareGroup,
        UpdateGroupWeight,
        CreateKey,
        UpdateKey,
        CreatedKey,
        SetDisabled,
        ListKeysQuery,
        crate::BudgetUsageQuery,
        UsageQuery,
        UsageSeriesQuery,
        UsageSpeedQuery,
        UsageDailyQuery,
        UsageLogQuery,
        KeyUsageSummaryQuery,
        KeyWatchQuery,
        KeySource,
        KeyRejectionAgg,
        UsageAgg,
        UsageKeyAgg,
        UsageModelAgg,
        KeyUsageSummary,
        UsageTimePoint,
        TenantUsageTimePoint,
        ModelUsageTimePoint,
        ModelSpeedPoint,
        UsageBreakdownQuery,
        UsageKeyModelBreakdown,
        CacheStats,
        UsageLogRow,
        UsageLogHistogram,
        UsageLogFacets,
        UsageFailureGroup,
        UsageFailureGroups,
        UsageLogFacet,
        UsageLogFailure,
        UsageLogBucket,
        UsageDailyRow,
        CostAgg,
        LiveStats,
        OverviewSummaryView,
        crate::OverviewView,
        crate::usage::OverviewTenantUsage,
        crate::usage::OverviewQuery,
        FairshareLiveView,
        CapacityDiscoveryView,
        CapacityDiscoveryModelView,
        crate::capacity_discovery::ModelCapacityStatus,
        crate::CapacityServicesView,
        crate::capacity_discovery::ServiceSummary,
        FairshareHistoryView,
        FairshareHistoryPointView,
        FairshareHistoryQuery,
        GroupFairshareView,
        TenantFairshareView,
        KeyFairshareView,
        ModelPoolView,
        CreateModel,
        UpdateModel,
        ModelVariantWrite,
        ModelVariant,
        SetModelCapacity,
        SetModelStatus,
        obleth_config::ModelLifecycle,
        obleth_config::ModelReasoning,
        obleth_config::ReasoningMode,
        obleth_config::ReasoningLevel,
        obleth_config::ReasoningAction,
        obleth_config::ModelFamily,
        obleth_config::FamilyMembers,
        crate::ModelFamilyWrite,
        crate::SetModelListing,
        crate::SetModelRejectedRequestFields,
        obleth_config::ModelSessionAffinity,
        obleth_config::ModelStatus,
        SetModelCapacityMode,
        ApplyAutotuneCapacity,
        SetModelWeight,
        SetModelCache,
        SetModelReliability,
        CreateModelEndpoint,
        UpdateModelEndpoint,
        ManagedModelSpec,
        ModelReplica,
        obleth_config::DeploymentLaunch,
        crate::DeploymentLaunchesQuery,
        crate::PutManagedModel,
        crate::CreateReplica,
        crate::PatchReplica,
        CreateMcpServer,
        UpdateMcpServer,
        AuditQuery,
        obleth_store::AuditEntry,
        crate::UsageLogEntry,
        crate::UsageBreakdownEntry,
        crate::batches::AdminBatchView,
        crate::batches::AdminBatchList,
        CapacityView,
        crate::replica_settings::CeilingSource,
        SetCapacity,
        UsageRetentionView,
        KeyPrefixView,
        UpdateKeyPrefix,
        crate::NetworkSettingsView,
        crate::ClientAddressView,
        crate::UpdateNamedNetworks,
        obleth_config::network::NamedNetwork,
        UpdateUsageRetention,
        CompactUsageResult,
        AlertSettingsView,
        EmailSettingsView,
        UpdateAlertSettings,
        UpdateEmailSettings,
        TestAlertResult,
        AutoRouterSettingsView,
        UpdateAutoRouterSettings,
        crate::SimulateRouteRequest,
        obleth_config::routing::route_explain::RouteExplain,
        obleth_config::routing::route_explain::ScoredCandidate,
        obleth_config::routing::route_explain::Rejection,
        obleth_config::routing::route_explain::WeightsView,
        obleth_config::routing::IntentSource,
        BoonSettingsView,
        UpdateBoonSettings,
        crate::knowledge::KnowledgeSettingsView,
        crate::knowledge::UpdateKnowledgeSettings,
        crate::knowledge::CollectionView,
        crate::knowledge::CreateCollection,
        crate::knowledge::UpdateCollection,
        crate::knowledge::DocumentView,
        crate::knowledge::UploadDocument,
        crate::knowledge::ReindexResult,
        crate::knowledge::SetModelCollections,
        crate::knowledge::ModelCollectionsView,
        EnergySettingsView,
        UpdateEnergySettings,
        TestEnergyQuery,
        EnergyTestResult,
        ModelPowerTest,
        ModelPowerMatch,
        CompressorStatusView,
        CharoSettingsView,
        obleth_config::SlurmSettings,
        obleth_config::NodeAlias,
        obleth_config::ClusterDefaults,
        crate::slurm_settings::SlurmSettingsView,
        crate::slurm_settings::UpdateSlurmSettings,
        crate::slurm_settings::SlurmHealthView,
        crate::slurm_settings::SlurmJwtHealth,
        crate::slurm_settings::SlurmPingHealth,
        crate::slurm_settings::SlurmApiHealth,
        crate::slurm_settings::SlurmAuthHealth,
        crate::alerts::ChannelResult,
        AutotuneRequest,
        AutotuneReport,
        AutotuneStep,
        KneeReason,
        WorkloadProfile,
        ConfigBackup,
        BackupEncryption,
        BackupData,
        FairshareGroupBackup,
        TenantBackup,
        ApiKeyBackup,
        ModelBackup,
        ModelEndpointBackup,
        McpServerBackup,
        AppSettingBackup,
        RestoreReport,
        RestoreCounts,
        ModelManifest,
        ManifestModel,
        ManifestEndpoint,
        ModelImportReport,
        ModelImportEntry,
        crate::VersionInfo,
        crate::ResyncReport,
        crate::SetKeyTracing,
        crate::SetKeyEndUserFairshare,
        crate::SetKeyBackground,
        crate::ProvisionErrorBody,
        crate::ManagedActionResult,
        obleth_config::ManagedBackoff,
        crate::usage::SpanEntry,
        crate::recipes::RecipeView,
        crate::recipes::UpsertRecipeBody,
        crate::advice::AdviceReport,
        crate::advice::AdviceFinding,
        crate::advice::AdviceJudge,
        crate::advice::AdviceEvidence,
        crate::advice::AdviceFix,
        crate::advice::AdviceJudgment,
        crate::advice::AdviceLogs,
        crate::advice::SnoozedAdvice,
        crate::advice::SnoozeAdvice,
        crate::knowledge::SearchQuery,
        crate::knowledge::SearchHit,
        crate::router_readiness::RouterReadiness,
        crate::router_readiness::ReadinessFinding,
        obleth_config::GuardrailsAction,
        obleth_config::ManagedModelBackup,
        obleth_config::RecipeBackup,
    )),
    tags(
        (name = "meta", description = "Gateway build/version identity and resolver-cache reconcile"),
        (name = "tenants", description = "Tenant lifecycle, quotas, and fairshare group assignment"),
        (name = "keys", description = "API key lifecycle and per-key usage summary"),
        (name = "usage", description = "Usage, cost, request logs, and live stats"),
        (name = "fairshare", description = "Live scheduler snapshot and group weights"),
        (name = "models", description = "Model registry, capacity, endpoints, health, and auto-tune"),
        (name = "mcp", description = "MCP server registry"),
        (name = "recipes", description = "Admin-authored recipe documents"),
        (name = "knowledge", description = "Institutional knowledge base: collections, documents, and re-indexing"),
        (name = "audit", description = "Admin audit log"),
        (name = "batches", description = "Batches across every tenant"),
        (name = "capacity", description = "Global gateway in-flight capacity"),
        (name = "settings", description = "Runtime alerting, auto-router, and retention configuration"),
        (name = "backup", description = "Configuration backup and restore"),
        (name = "slurm", description = "Live Slurm cluster resource discovery")
    )
)]
pub struct ApiDoc;

#[cfg(test)]
mod tests {
    use super::ApiDoc;
    use utoipa::OpenApi;

    /// Routes the document leaves out on purpose: the liveness probe, the
    /// document itself, and the control-plane key, which only the bundled
    /// dashboard reads.
    const UNDOCUMENTED: [(&str, &str); 3] = [
        ("get", "/api/v1/health"),
        ("get", "/api/v1/openapi.json"),
        ("get", "/api/v1/system/control-plane-key"),
    ];

    /// Old verbs still routed beside the documented `PUT`, for one release.
    const ALIASES: [(&str, &str); 9] = [
        ("patch", "/api/v1/tenants/{id}/status"),
        ("patch", "/api/v1/tenants/{id}/schedule"),
        ("patch", "/api/v1/tenants/{id}/budget"),
        ("patch", "/api/v1/tenants/{id}/allowlist"),
        ("patch", "/api/v1/tenants/{id}/guardrails"),
        ("patch", "/api/v1/tenants/{id}/compression"),
        ("patch", "/api/v1/tenants/{id}/weight"),
        ("patch", "/api/v1/tenants/{id}/group"),
        ("patch", "/api/v1/fairshare/groups/{name}/weight"),
    ];

    /// Every `(method, path)` the router in lib.rs serves, with `:param`
    /// written as `{param}` like the document.
    fn routed() -> Vec<(String, String)> {
        let src = include_str!("lib.rs");
        let start = src.find("pub fn router(").expect("router()");
        let end = start + src[start..].find("public.merge(").expect("router end");
        let mut out = Vec::new();
        let mut rest = &src[start..end];
        while let Some(at) = rest.find(".route(") {
            rest = &rest[at + ".route(".len()..];
            // The call's arguments run to the parenthesis that closes `.route(`.
            let mut depth = 1;
            let close = rest
                .char_indices()
                .find(|&(_, c)| {
                    depth += match c {
                        '(' => 1,
                        ')' => -1,
                        _ => 0,
                    };
                    depth == 0
                })
                .expect("closed .route(")
                .0;
            let call = &rest[..close];
            let path = call.split('"').nth(1).expect("route path");
            let path = path
                .split('/')
                .map(|seg| match seg.strip_prefix(':') {
                    Some(name) => format!("{{{name}}}"),
                    None => seg.to_string(),
                })
                .collect::<Vec<_>>()
                .join("/");
            // A method router is `get(` at the start or after a `.`, never
            // the tail of a handler name like `knowledge::get_collection(`.
            for (at, _) in call.match_indices('(') {
                let head = &call[..at];
                let word_start = head
                    .rfind(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                    .map_or(0, |i| i + 1);
                let word = &head[word_start..];
                let before = head[..word_start].trim_end().chars().last();
                if ["get", "post", "put", "patch", "delete"].contains(&word)
                    && matches!(before, Some(',' | '.'))
                {
                    out.push((word.to_string(), path.clone()));
                }
            }
        }
        out
    }

    #[test]
    fn every_route_is_documented_with_a_declared_tag_and_its_path_parameters() {
        let doc = serde_json::to_value(ApiDoc::openapi()).expect("serialize");
        let tags: Vec<&str> = doc["tags"]
            .as_array()
            .expect("tags")
            .iter()
            .filter_map(|t| t["name"].as_str())
            .collect();
        let routes = routed();
        assert!(
            routes.len() > 150,
            "the route scan found only {}",
            routes.len()
        );
        let mut problems = Vec::new();
        for (method, path) in &routes {
            if UNDOCUMENTED.contains(&(method.as_str(), path.as_str())) {
                continue;
            }
            if ALIASES.contains(&(method.as_str(), path.as_str())) {
                if !doc["paths"][path]["put"].is_object() {
                    problems.push(format!("{method} {path}: an alias of an undocumented put"));
                }
                continue;
            }
            let op = &doc["paths"][path][method];
            if !op.is_object() {
                problems.push(format!("{method} {path}: not in paths(...)"));
                continue;
            }
            match op["tags"].as_array().map(Vec::as_slice) {
                Some([tag]) if tags.contains(&tag.as_str().unwrap_or_default()) => {}
                other => problems.push(format!("{method} {path}: tags {other:?}")),
            }
            let params: Vec<&str> = op["parameters"]
                .as_array()
                .map(|ps| {
                    ps.iter()
                        .filter(|p| p["in"] == "path")
                        .filter_map(|p| p["name"].as_str())
                        .collect()
                })
                .unwrap_or_default();
            for seg in path.split('/') {
                if let Some(name) = seg.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
                    if !params.contains(&name) {
                        problems.push(format!(
                            "{method} {path}: path parameter {name} undocumented"
                        ));
                    }
                }
            }
        }
        // And nothing documented that no route serves.
        for (path, item) in doc["paths"].as_object().expect("paths") {
            for method in item.as_object().expect("path item").keys() {
                if !routes.iter().any(|(m, p)| m == method && p == path) {
                    problems.push(format!("{method} {path}: documented but not routed"));
                }
            }
        }
        assert!(problems.is_empty(), "{}", problems.join("\n"));
    }

    #[test]
    fn every_schema_the_document_references_is_registered() {
        fn refs<'a>(v: &'a serde_json::Value, out: &mut Vec<&'a str>) {
            match v {
                serde_json::Value::Object(m) => {
                    if let Some(r) = m.get("$ref").and_then(|r| r.as_str()) {
                        out.push(r);
                    }
                    m.values().for_each(|v| refs(v, out));
                }
                serde_json::Value::Array(a) => a.iter().for_each(|v| refs(v, out)),
                _ => {}
            }
        }
        let doc = serde_json::to_value(ApiDoc::openapi()).expect("serialize");
        let mut all = Vec::new();
        refs(&doc, &mut all);
        let missing: std::collections::BTreeSet<&str> = all
            .into_iter()
            .filter_map(|r| r.strip_prefix("#/components/schemas/"))
            .filter(|name| doc["components"]["schemas"].get(*name).is_none())
            .collect();
        assert!(
            missing.is_empty(),
            "referenced but not in components(...): {missing:?}"
        );
    }

    #[test]
    fn model_route_view_documents_its_fields() {
        let doc = serde_json::to_value(ApiDoc::openapi()).expect("serialize");
        let props = &doc["components"]["schemas"]["ModelRouteView"]["properties"];
        for field in [
            "model_name",
            "aliases",
            "variants",
            "quantization",
            "route_bias",
            "auto_eligible",
        ] {
            let description = props[field]["description"].as_str().unwrap_or_default();
            assert!(
                !description.is_empty(),
                "ModelRouteView.{field} has no description"
            );
        }
    }

    #[test]
    fn model_writes_document_variants() {
        let doc = serde_json::to_value(ApiDoc::openapi()).expect("serialize");
        let schemas = &doc["components"]["schemas"];
        for write in ["CreateModel", "UpdateModel"] {
            let description = schemas[write]["properties"]["variants"]["description"]
                .as_str()
                .unwrap_or_default();
            assert!(
                !description.is_empty(),
                "{write}.variants has no description"
            );
        }
        for field in ["name", "description", "boons"] {
            assert!(
                schemas["ModelVariantWrite"]["properties"][field].is_object(),
                "ModelVariantWrite.{field} is not documented"
            );
            assert!(
                schemas["ModelVariant"]["properties"][field].is_object(),
                "ModelVariant.{field} is not documented"
            );
        }
        // The request log documents the new columns.
        for field in [
            "parent_request_id",
            "model_variant",
            "client_ip",
            "client_country",
            "user_agent",
        ] {
            assert!(
                schemas["UsageLogRow"]["properties"][field].is_object(),
                "UsageLogRow.{field} is not documented"
            );
        }
    }
}
