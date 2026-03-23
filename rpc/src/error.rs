use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

#[derive(Debug, thiserror::Error)]
pub enum RpcError {
    #[error("not found: {0}")]
    NotFound(String),

    #[error("bad request: {0}")]
    BadRequest(String),

    #[error("internal error: {0}")]
    Internal(String),

    #[error("storage error: {0}")]
    Storage(#[from] nusantara_storage::StorageError),

    #[error("deserialization error: {0}")]
    Deserialization(String),

    #[error("faucet disabled")]
    FaucetDisabled,

    #[error("rate limited: {0}")]
    RateLimited(String),

    #[error("request timed out: {0}")]
    Timeout(String),
}

#[derive(Serialize)]
struct ErrorBody {
    error: String,
}

impl IntoResponse for RpcError {
    fn into_response(self) -> Response {
        let status = match &self {
            RpcError::NotFound(_) => StatusCode::NOT_FOUND,
            RpcError::BadRequest(_) => StatusCode::BAD_REQUEST,
            RpcError::FaucetDisabled => StatusCode::SERVICE_UNAVAILABLE,
            RpcError::RateLimited(_) => StatusCode::TOO_MANY_REQUESTS,
            RpcError::Timeout(_) => StatusCode::GATEWAY_TIMEOUT,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };

        let body = ErrorBody {
            error: self.to_string(),
        };

        (status, axum::Json(body)).into_response()
    }
}
