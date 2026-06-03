//! OpenAPI document for the Management API (served at `/api/v1/openapi.json`).

use utoipa::OpenApi;

use crate::model_health::{BulkModelHealthResult, UpdateModelHealthConfig};
use crate::usage::UsageAgg;
use crate::{
    CapacityView, CreateKey, CreateTenant, CreatedKey, SetCapacity, SetDisabled, UpdateQuota,
    UpdateWeight,
};
use obleth_config::{ApiKey, ModelHealthCheck, ModelHealthDetail, ModelHealthSummary, Tenant};

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
        crate::patch_weight,
        crate::create_key,
        crate::get_usage,
        crate::model_health::list_health,
        crate::model_health::get_health,
        crate::model_health::check_one,
        crate::model_health::check_all,
        crate::model_health::update_config,
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
        UpdateWeight,
        UpdateQuota,
        CreateKey,
        CreatedKey,
        SetDisabled,
        SetCapacity,
        CapacityView,
        UsageAgg,
    )),
    tags(
        (name = "tenants", description = "Tenant + fairshare weight management"),
        (name = "keys", description = "API key lifecycle"),
        (name = "usage", description = "Usage and cost analytics")
    )
)]
pub struct ApiDoc;
