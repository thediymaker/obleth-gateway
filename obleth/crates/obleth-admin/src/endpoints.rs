//! Model endpoints.

use super::*;

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateModelEndpoint {
    pub name: String,
    pub api_base: String,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default = "default_endpoint_priority")]
    pub priority: i64,
    #[serde(default = "default_endpoint_weight")]
    pub weight: i64,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Requests this endpoint takes at once, for a `discovered` model using
    /// the `endpoints` source. Omitted uses the model's
    /// `per_replica_max_in_flight`.
    #[serde(default)]
    pub max_in_flight: Option<i64>,
}

/// Body of `PUT /api/v1/models/{id}/endpoints/{endpoint_id}`. An omitted
/// field keeps its stored value; `null` clears the key or the cap.
#[derive(Debug, Default, Deserialize, ToSchema)]
pub struct UpdateModelEndpoint {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub api_base: Option<String>,
    /// Write-only. `null` or an empty string clears the stored key.
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<String>)]
    pub api_key: Option<Option<String>>,
    #[serde(default)]
    pub priority: Option<i64>,
    #[serde(default)]
    pub weight: Option<i64>,
    #[serde(default)]
    pub enabled: Option<bool>,
    /// `null` lifts the endpoint's own cap.
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<i64>)]
    pub max_in_flight: Option<Option<i64>>,
}

fn default_endpoint_priority() -> i64 {
    100
}

fn default_endpoint_weight() -> i64 {
    100
}

#[utoipa::path(
    get, path = "/api/v1/models/{id}/endpoints", tag = "models",
    params(("id" = Uuid, Path, description = "Model id")),
    responses((status = 200, body = [ModelEndpointView]))
)]
pub(crate) async fn list_model_endpoints(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<ModelEndpointView>>> {
    // Confirm the model exists so callers get 404 (not an empty list) for a
    // bad id.
    state.store.get_model(id).await?;
    Ok(Json(views(state.store.list_model_endpoints(id).await?)))
}

/// Every model's endpoints in one read, with their health, ordered by model
/// and then priority. The provisioner reads this once a tick.
#[utoipa::path(
    get, path = "/api/v1/endpoints", tag = "models",
    responses((status = 200, body = [ModelEndpointView]))
)]
pub(crate) async fn list_all_endpoints(
    State(state): State<AdminState>,
) -> Result<Json<Vec<ModelEndpointView>>> {
    Ok(Json(views(state.store.all_model_endpoints().await?)))
}

/// An endpoint's own concurrency, when given, has the same bounds as a
/// model's `per_replica_max_in_flight`.
fn validate_endpoint_max_in_flight(value: Option<i64>) -> Result<()> {
    obleth_config::capacity::validate_per_replica_max_in_flight(value).map_err(|_| {
        AdminError::BadRequest(format!(
            "max_in_flight must be between 1 and {}",
            obleth_config::capacity::MAX_PER_REPLICA_MAX_IN_FLIGHT
        ))
    })
}

#[utoipa::path(
    post, path = "/api/v1/models/{id}/endpoints", tag = "models",
    params(("id" = Uuid, Path, description = "Model id")),
    request_body = CreateModelEndpoint,
    responses((status = 200, body = ModelEndpointView))
)]
pub(crate) async fn create_model_endpoint(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<CreateModelEndpoint>,
) -> Result<Json<ModelEndpointView>> {
    state.ssrf.validate(&body.api_base).await?;
    validate_endpoint_max_in_flight(body.max_in_flight)?;
    let model = state.store.get_model(id).await?;
    validate_endpoint_write(&state, &model, None, body.max_in_flight).await?;
    let endpoint = state
        .store
        .create_model_endpoint(
            id,
            &body.name,
            &body.api_base,
            body.api_key.as_deref(),
            body.priority,
            body.weight,
            body.enabled,
            body.max_in_flight,
        )
        .await?;
    sync_model(&state, &model).await?;
    state
        .audit(
            &headers,
            "create_model_endpoint",
            "model_endpoint",
            endpoint.id,
            serde_json::json!({
                "model": model.model_name,
                "name": endpoint.name,
                "api_base": endpoint.api_base,
            }),
        )
        .await?;
    Ok(Json(endpoint.into()))
}

#[utoipa::path(
    put, path = "/api/v1/models/{id}/endpoints/{endpoint_id}", tag = "models",
    params(
        ("id" = Uuid, Path, description = "Model id"),
        ("endpoint_id" = Uuid, Path, description = "Endpoint id")
    ),
    request_body = UpdateModelEndpoint,
    responses((status = 200, body = ModelEndpointView), (status = 404))
)]
pub(crate) async fn update_model_endpoint(
    State(state): State<AdminState>,
    Path((id, endpoint_id)): Path<(Uuid, Uuid)>,
    headers: HeaderMap,
    Json(body): Json<UpdateModelEndpoint>,
) -> Result<Json<ModelEndpointView>> {
    let model = state.store.get_model(id).await?;
    // An id outside this model finds nothing here: a 404, like the scoped
    // update below would give.
    let existing = state
        .store
        .list_model_endpoints(model.id)
        .await?
        .into_iter()
        .find(|e| e.id == endpoint_id)
        .ok_or(obleth_store::StoreError::NotFound)?;
    let api_base = body.api_base.unwrap_or(existing.api_base);
    state.ssrf.validate(&api_base).await?;
    let max_in_flight = body.max_in_flight.unwrap_or(existing.max_in_flight);
    validate_endpoint_max_in_flight(max_in_flight)?;
    validate_endpoint_write(&state, &model, Some(endpoint_id), max_in_flight).await?;
    // The store keeps the secret on `None` and clears it on an empty string.
    let api_key = body.api_key.map(Option::unwrap_or_default);
    let endpoint = state
        .store
        // Scoped to the path model: another model's endpoint id is a 404.
        .update_model_endpoint(
            model.id,
            endpoint_id,
            body.name.as_deref().unwrap_or(&existing.name),
            &api_base,
            api_key.as_deref(),
            body.priority.unwrap_or(existing.priority),
            body.weight.unwrap_or(existing.weight),
            body.enabled.unwrap_or(existing.enabled),
            max_in_flight,
        )
        .await?;
    sync_model(&state, &model).await?;
    state
        .audit(
            &headers,
            "update_model_endpoint",
            "model_endpoint",
            endpoint.id,
            serde_json::json!({
                "model": model.model_name,
                "name": endpoint.name,
                "api_base": endpoint.api_base,
                "enabled": endpoint.enabled,
            }),
        )
        .await?;
    Ok(Json(endpoint.into()))
}

#[utoipa::path(
    delete, path = "/api/v1/models/{id}/endpoints/{endpoint_id}", tag = "models",
    params(
        ("id" = Uuid, Path, description = "Model id"),
        ("endpoint_id" = Uuid, Path, description = "Endpoint id")
    ),
    responses((status = 204), (status = 404))
)]
pub(crate) async fn delete_model_endpoint(
    State(state): State<AdminState>,
    Path((id, endpoint_id)): Path<(Uuid, Uuid)>,
    headers: HeaderMap,
) -> Result<StatusCode> {
    let model = state.store.get_model(id).await?;
    // Scoped to the path model: another model's endpoint id is a 404.
    state
        .store
        .delete_model_endpoint(model.id, endpoint_id)
        .await?;
    sync_model(&state, &model).await?;
    state
        .audit(
            &headers,
            "delete_model_endpoint",
            "model_endpoint",
            endpoint_id,
            serde_json::json!({ "model": model.model_name }),
        )
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
