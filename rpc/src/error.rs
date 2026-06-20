use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

#[derive(Debug, thiserror::Error)]
pub enum RpcError {
    #[error("not found: {0}")]
    NotFound(String),

    #[error("bad request: {0}")]
    BadRequest(String),

    /// Internal server errors: the inner detail is logged server-side with a
    /// correlation ID but is NOT returned to the client to prevent information
    /// leakage (F27).
    #[error("internal error: {0}")]
    Internal(String),

    /// Storage errors are treated as internal errors for the same reason.
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
    /// Correlation ID for matching this response to a server-side `tracing::error!` log.
    /// Only present on 500-class responses.
    #[serde(skip_serializing_if = "Option::is_none")]
    trace_id: Option<u64>,
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

        let body = if status == StatusCode::INTERNAL_SERVER_ERROR {
            // Generate a random correlation ID so operators can find the
            // corresponding `tracing::error!` log entry.
            let trace_id: u64 = {
                use std::time::{SystemTime, UNIX_EPOCH};
                // Use timestamp-based pseudo-random ID (no external rand dep needed).
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.subsec_nanos() as u64 ^ (d.as_secs().wrapping_mul(0x9e37_79b9_7f4a_7c15)))
                    .unwrap_or(0xdead_beef_cafe_babe)
            };

            // Log full detail server-side with the correlation ID.
            tracing::error!(
                trace_id = trace_id,
                error = %self,
                "RPC internal error"
            );

            ErrorBody {
                error: "internal error".to_string(),
                trace_id: Some(trace_id),
            }
        } else {
            ErrorBody {
                error: self.to_string(),
                trace_id: None,
            }
        };

        (status, axum::Json(body)).into_response()
    }
}
