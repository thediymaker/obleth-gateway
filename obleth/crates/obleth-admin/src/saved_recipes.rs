//! CRUD for shared launcher recipes (the dashboard "Save as recipe" feature).

use axum::extract::{Path, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{AdminError, AdminState, Result};

#[derive(Serialize, ToSchema)]
pub struct RecipeView {
    pub id: Uuid,
    pub name: String,
    pub backend: String,
    pub author: String,
    #[schema(value_type = Object)]
    pub spec: serde_json::Value,
}

#[derive(Deserialize, ToSchema)]
pub struct UpsertRecipeBody {
    pub name: String,
    #[serde(default)] pub backend: String,
    #[serde(default)] pub author: String,
    #[serde(default)]
    #[schema(value_type = Object)]
    pub spec: serde_json::Value,
}

fn view(r: obleth_store::SavedRecipe) -> RecipeView {
    RecipeView { id: r.id, name: r.name, backend: r.backend, author: r.author, spec: r.spec }
}

#[utoipa::path(get, path = "/api/v1/recipes", responses((status = 200, body = [RecipeView])))]
pub async fn list_recipes(State(state): State<AdminState>) -> Result<Json<Vec<RecipeView>>> {
    Ok(Json(state.store.list_saved_recipes().await?.into_iter().map(view).collect()))
}

#[utoipa::path(post, path = "/api/v1/recipes", request_body = UpsertRecipeBody,
    responses((status = 200, body = RecipeView)))]
pub async fn create_recipe(
    State(state): State<AdminState>, Json(b): Json<UpsertRecipeBody>,
) -> Result<Json<RecipeView>> {
    if b.name.trim().is_empty() { return Err(AdminError::BadRequest("name required".into())); }
    let r = state.store.upsert_saved_recipe(obleth_store::UpsertSavedRecipe {
        id: None, name: b.name, backend: b.backend, author: b.author, spec: b.spec,
    }).await?;
    Ok(Json(view(r)))
}

#[utoipa::path(put, path = "/api/v1/recipes/{id}", request_body = UpsertRecipeBody,
    responses((status = 200, body = RecipeView)))]
pub async fn update_recipe(
    State(state): State<AdminState>, Path(id): Path<Uuid>, Json(b): Json<UpsertRecipeBody>,
) -> Result<Json<RecipeView>> {
    if b.name.trim().is_empty() { return Err(AdminError::BadRequest("name required".into())); }
    let r = state.store.upsert_saved_recipe(obleth_store::UpsertSavedRecipe {
        id: Some(id), name: b.name, backend: b.backend, author: b.author, spec: b.spec,
    }).await?;
    Ok(Json(view(r)))
}

#[utoipa::path(delete, path = "/api/v1/recipes/{id}", responses((status = 200)))]
pub async fn delete_recipe(
    State(state): State<AdminState>, Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>> {
    state.store.delete_saved_recipe(id).await?;
    Ok(Json(serde_json::json!({"deleted": true})))
}
