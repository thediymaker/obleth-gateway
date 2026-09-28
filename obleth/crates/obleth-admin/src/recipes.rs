//! CRUD for admin-authored recipe templates (raw recipe documents).

use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::Json;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{audit_actor, AdminError, AdminState, Result};

#[derive(Serialize, ToSchema)]
pub struct RecipeView {
    pub id: Uuid,
    pub name: String,
    pub body: String,
    pub author: String,
}

#[derive(Deserialize, ToSchema)]
pub struct UpsertRecipeBody {
    pub name: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub author: String,
}

fn view(r: obleth_store::Recipe) -> RecipeView {
    RecipeView {
        id: r.id,
        name: r.name,
        body: r.body,
        author: r.author,
    }
}

/// Audit detail for a recipe write: identity fields plus the body size, not
/// the (possibly large) document itself.
fn audit_detail(r: &RecipeView) -> serde_json::Value {
    serde_json::json!({
        "name": r.name,
        "author": r.author,
        "body_bytes": r.body.len(),
    })
}

#[utoipa::path(get, path = "/api/v1/recipes", responses((status = 200, body = [RecipeView])))]
pub async fn list_recipes(State(state): State<AdminState>) -> Result<Json<Vec<RecipeView>>> {
    Ok(Json(
        state
            .store
            .list_recipes()
            .await?
            .into_iter()
            .map(view)
            .collect(),
    ))
}

#[utoipa::path(post, path = "/api/v1/recipes", request_body = UpsertRecipeBody,
    responses((status = 200, body = RecipeView)))]
pub async fn create_recipe(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Json(b): Json<UpsertRecipeBody>,
) -> Result<Json<RecipeView>> {
    if b.name.trim().is_empty() {
        return Err(AdminError::BadRequest("name required".into()));
    }
    let r = view(
        state
            .store
            .upsert_recipe(obleth_store::UpsertRecipe {
                id: None,
                name: b.name,
                body: b.body,
                author: b.author,
            })
            .await?,
    );
    state
        .store
        .record_audit(
            &audit_actor(&headers),
            "create_recipe",
            "recipe",
            &r.id.to_string(),
            audit_detail(&r),
        )
        .await?;
    Ok(Json(r))
}

#[utoipa::path(put, path = "/api/v1/recipes/{id}", request_body = UpsertRecipeBody,
    responses((status = 200, body = RecipeView)))]
pub async fn update_recipe(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<UpsertRecipeBody>,
) -> Result<Json<RecipeView>> {
    if b.name.trim().is_empty() {
        return Err(AdminError::BadRequest("name required".into()));
    }
    let r = view(
        state
            .store
            .upsert_recipe(obleth_store::UpsertRecipe {
                id: Some(id),
                name: b.name,
                body: b.body,
                author: b.author,
            })
            .await?,
    );
    state
        .store
        .record_audit(
            &audit_actor(&headers),
            "update_recipe",
            "recipe",
            &r.id.to_string(),
            audit_detail(&r),
        )
        .await?;
    Ok(Json(r))
}

#[utoipa::path(delete, path = "/api/v1/recipes/{id}", responses((status = 200)))]
pub async fn delete_recipe(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>> {
    state.store.delete_recipe(id).await?;
    state
        .store
        .record_audit(
            &audit_actor(&headers),
            "delete_recipe",
            "recipe",
            &id.to_string(),
            serde_json::json!({}),
        )
        .await?;
    Ok(Json(serde_json::json!({"deleted": true})))
}
