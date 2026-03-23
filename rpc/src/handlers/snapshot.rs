use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use nusantara_storage::snapshot_archive::find_latest_snapshot_file;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::server::RpcState;

#[derive(Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotResponse {
    pub slot: u64,
    pub bank_hash: String,
    pub account_count: u64,
    pub timestamp: i64,
    /// SHA3-512 hash of the snapshot file, base64 URL-safe no-pad encoded.
    /// `None` if the snapshot file is not found on disk.
    pub file_hash: Option<String>,
    /// Size of the snapshot file in bytes.
    /// `None` if the snapshot file is not found on disk.
    pub file_size: Option<u64>,
}

/// Get the latest snapshot info.
#[utoipa::path(
    get,
    path = "/v1/snapshot/latest",
    responses(
        (status = 200, description = "Latest snapshot info", body = SnapshotResponse),
        (status = 404, description = "No snapshot available"),
    )
)]
#[tracing::instrument(skip(state))]
pub async fn get_latest_snapshot(State(state): State<Arc<RpcState>>) -> impl IntoResponse {
    match state.storage.get_latest_snapshot() {
        Ok(Some(manifest)) => {
            // Attempt to locate the snapshot file on disk and compute its hash + size
            let (file_hash, file_size) = match find_latest_snapshot_file(&state.snapshot_dir) {
                Some(path) => match std::fs::read(&path) {
                    Ok(bytes) => {
                        let hash = nusantara_crypto::hash(&bytes);
                        let size = bytes.len() as u64;
                        (Some(hash.to_base64()), Some(size))
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "failed to read snapshot file for hashing");
                        (None, None)
                    }
                },
                None => (None, None),
            };

            let resp = SnapshotResponse {
                slot: manifest.slot,
                bank_hash: manifest.bank_hash.to_base64(),
                account_count: manifest.account_count,
                timestamp: manifest.timestamp,
                file_hash,
                file_size,
            };
            (StatusCode::OK, Json(serde_json::json!(resp))).into_response()
        }
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "no snapshot available"})),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}
