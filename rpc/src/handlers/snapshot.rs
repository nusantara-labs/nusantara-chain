use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use nusantara_storage::snapshot_archive::find_latest_snapshot_file;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::server::{CachedSnapshotInfo, RpcState};

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

/// Hash a snapshot file asynchronously using `spawn_blocking`.
///
/// Checks the LRU snapshot cache first (keyed by path + mtime + size) to
/// avoid rehashing large files on every request.  The cache is bounded to
/// `SNAPSHOT_CACHE_CAPACITY` entries.
async fn hash_snapshot_file(
    state: &RpcState,
    path: PathBuf,
) -> Option<(String, u64)> {
    // Check metadata to determine whether the cache entry is still valid.
    let meta = tokio::fs::metadata(&path).await.ok()?;
    let size = meta.len();
    let mtime = meta.modified().ok().unwrap_or(SystemTime::UNIX_EPOCH);

    // Cache hit: return the stored hash without re-reading the file.
    {
        let mut cache = state.snapshot_cache.lock();
        if let Some(entry) = cache.get(&path)
            && entry.mtime == mtime
            && entry.size == size
        {
            return Some((entry.hash.to_base64(), size));
        }
    }

    // Cache miss: hash the file in a blocking task (file I/O, potentially GiBs).
    let path_clone = path.clone();
    let hash = tokio::task::spawn_blocking(move || -> Option<nusantara_crypto::Hash> {
        // Stream through 1 MiB chunks to avoid loading multi-GB files into RAM.
        use std::io::Read;
        let file = std::fs::File::open(&path_clone).ok()?;
        let mut reader = std::io::BufReader::with_capacity(1 << 20, file);
        let mut hasher = nusantara_crypto::Hasher::new();
        let mut buf = vec![0u8; 1 << 20];
        loop {
            let n = reader.read(&mut buf).ok()?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
        Some(hasher.finalize())
    })
    .await
    .ok()??;

    // Store in LRU cache.
    {
        let mut cache = state.snapshot_cache.lock();
        cache.put(path, CachedSnapshotInfo { mtime, size, hash });
    }

    Some((hash.to_base64(), size))
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
            // Locate the snapshot file and compute its hash + size asynchronously.
            let (file_hash, file_size) =
                match find_latest_snapshot_file(&state.snapshot_dir) {
                    Ok(Some(path)) => {
                        match hash_snapshot_file(&state, path).await {
                            Some((hash, size)) => (Some(hash), Some(size)),
                            None => {
                                tracing::warn!("failed to hash snapshot file");
                                (None, None)
                            }
                        }
                    }
                    Ok(None) => (None, None),
                    Err(e) => {
                        tracing::warn!(error = %e, "failed to scan snapshot directory");
                        (None, None)
                    }
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
        Err(e) => {
            tracing::error!(error = %e, "failed to fetch latest snapshot from storage");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "internal error"})),
            )
                .into_response()
        }
    }
}
