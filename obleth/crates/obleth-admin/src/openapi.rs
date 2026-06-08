//! OpenAPI document for the Management API (served at `/api/v1/openapi.json`).

use utoipa::OpenApi;

use crate::model_health::{BulkModelHealthResult, UpdateModelHealthConfig};
use crate::usage::{UsageAgg, UsageDailyRow};
use crate::{
    AlertSettingsView, EmailSettingsView, TestAlertResult, UpdateAlertSettings, UpdateEmailSettings,
};
use crate::{
    CapacityView, CompactUsageResult, CreateKey, CreateTenant, CreatedKey, SetCapacity,
    SetDisabled, SetTenantAllowlist, SetTenantBudget, SetTenantSchedule, SetTenantStatus,
    UpdateQuota, UpdateTenant, UpdateUsageRetention, UpdateWeight, UsageRetentionView,
};
use crate::autotune::{AutotuneReport, AutotuneRequest, AutotuneStep, KneeReason, WorkloadProfile};
use crate::{ApplyAutotuneCapacity, SetModelCapacityMode};
use obleth_config::{
    ApiKey, ModelHealthCheck, ModelHealthDetail, ModelHealthSummary, ModelRoute, Tenant,
    WeeklyWindow,
};

#[derive(OpenApi)]
#[openapi(
    info(
        title = "obleth Management API",
        version = "0.1.0",
        description = "Programmatic control plane: tenants, keys, weights, quotas, usage."
    ),
    paths(
        crate::create_tenant,
        crate::list_tenants,
        crate::update_tenant,
        crate::patch_tenant_status,
        crate::patch_tenant_schedule,
        crate::patch_tenant_budget,
        crate::patch_tenant_allowlist,
        crate::delete_tenant,
        crate::patch_weight,
        crate::create_key,
        crate::get_usage,
        crate::get_usage_daily,
        crate::compact_usage,
        crate::get_alert_settings,
        crate::put_alert_settings,
        crate::test_alert_settings,
        crate::get_usage_retention,
        crate::put_usage_retention,
        crate::model_health::list_health,
        crate::model_health::get_health,
        crate::model_health::check_one,
        crate::model_health::check_all,
        crate::model_health::update_config,
        crate::set_model_capacity_mode,
        crate::autotune_model,
        crate::apply_autotune_capacity,
    ),
    components(schemas(
        Tenant,
        ApiKey,
        ModelHealthSummary,
        ModelHealthCheck,
        ModelHealthDetail,
        UpdateModelHealthConfig,
        BulkModelHealthResult,
        CreateTenant,
        UpdateTenant,
        SetTenantStatus,
        SetTenantSchedule,
        SetTenantBudget,
        SetTenantAllowlist,
        WeeklyWindow,
        UpdateWeight,
        UpdateQuota,
        CreateKey,
        CreatedKey,
        SetDisabled,
        SetCapacity,
        CapacityView,
        UsageAgg,
        UsageDailyRow,
        UsageRetentionView,
        UpdateUsageRetention,
        CompactUsageResult,
        AlertSettingsView,
        EmailSettingsView,
        UpdateAlertSettings,
        UpdateEmailSettings,
        TestAlertResult,
        crate::alerts::ChannelResult,
        ModelRoute,
        SetModelCapacityMode,
        ApplyAutotuneCapacity,
        AutotuneRequest,
        AutotuneReport,
        AutotuneStep,
        KneeReason,
        WorkloadProfile,
    )),
    tags(
        (name = "tenants", description = "Tenant + fairshare weight management"),
        (name = "keys", description = "API key lifecycle"),
        (name = "models", description = "Model registry, capacity, and auto-tune"),
        (name = "usage", description = "Usage and cost analytics"),
        (name = "settings", description = "Runtime alerting configuration")
    )
)]
pub struct ApiDoc;
