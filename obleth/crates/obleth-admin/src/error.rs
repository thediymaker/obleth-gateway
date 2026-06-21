//! Management API error type with HTTP mapping.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;

#[derive(Debug, thiserror::Error)]
pub enum AdminError {
    #[error("unauthorized")]
    Unauthorized,
    #[error("not found")]
    NotFound,
    #[error("{0}")]
    BadRequest(String),
    #[error("store: {0}")]
    Store(#[from] obleth_store::StoreError),
    #[error("redis: {0}")]
    Redis(#[from] obleth_redis::RedisError),
    #[error("clickhouse: {0}")]
    Click(#[from] clickhouse::error::Error),
    #[error("{0}")]
    Internal(String),
}

impl From<crate::ssrf::SsrfError> for AdminError {
    fn from(e: crate::ssrf::SsrfError) -> Self {
        AdminError::BadRequest(e.to_string())
    }
}

impl IntoResponse for AdminError {
    fn into_response(self) -> Response {
        let status = match &self {
            AdminError::Unauthorized => StatusCode::UNAUTHORIZED,
            AdminError::NotFound => StatusCode::NOT_FOUND,
            AdminError::Store(obleth_store::StoreError::NotFound) => StatusCode::NOT_FOUND,
            AdminError::Store(obleth_store::StoreError::Conflict(_)) => StatusCode::CONFLICT,
            AdminError::Store(obleth_store::StoreError::Protected(_)) => StatusCode::FORBIDDEN,
            AdminError::BadRequest(_) => StatusCode::BAD_REQUEST,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        let body = Json(serde_json::json!({ "error": self.to_string() }));
        (status, body).into_response()
    }
}
