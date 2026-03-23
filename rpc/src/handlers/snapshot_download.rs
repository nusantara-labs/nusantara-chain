use std::sync::Arc;

use axum::body::Body;
use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use nusantara_storage::snapshot_archive::find_latest_snapshot_file;
use tokio_util::io::ReaderStream;

use crate::server::RpcState;

/// Download the latest snapshot as a binary file stream.
///
/// Locates the most recent `snapshot-{slot}.bin` file in the configured
/// snapshot directory and streams it back with `Content-Type: application/octet-stream`.
/// Returns 404 if no snapshot file is available.
#[utoipa::path(
    get,
    path = "/v1/snapshot/download",
    responses(
        (status = 200, description = "Snapshot binary file", content_type = "application/octet-stream"),
        (status = 404, description = "No snapshot file available"),
        (status = 500, description = "Internal server error"),
    )
)]
#[tracing::instrument(skip(state))]
pub async fn download_snapshot(State(state): State<Arc<RpcState>>) -> impl IntoResponse {
    let snapshot_path = match find_latest_snapshot_file(&state.snapshot_dir) {
        Some(path) => path,
        None => {
            return (
                StatusCode::NOT_FOUND,
                axum::Json(serde_json::json!({"error": "no snapshot file available"})),
            )
                .into_response();
        }
    };

    let file = match tokio::fs::File::open(&snapshot_path).await {
        Ok(f) => f,
        Err(e) => {
            tracing::error!(error = %e, path = %snapshot_path.display(), "failed to open snapshot file");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                axum::Json(serde_json::json!({"error": format!("failed to open snapshot: {e}")})),
            )
                .into_response();
        }
    };

    // Extract the filename for the Content-Disposition header
    let filename = snapshot_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("snapshot.bin")
        .to_string();

    let stream = ReaderStream::new(file);
    let body = Body::from_stream(stream);

    metrics::counter!("nusantara_rpc_snapshot_downloads").increment(1);

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{filename}\""),
        )
        .body(body)
        .unwrap_or_else(|e| {
            tracing::error!(error = %e, "failed to build snapshot download response");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        })
}
