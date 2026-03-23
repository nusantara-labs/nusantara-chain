//! Snapshot fetcher for validator bootstrap.
//!
//! Queries entrypoint RPC servers for the latest available snapshot,
//! downloads the binary file, verifies its SHA3-512 hash, and saves it
//! to the local snapshots directory so the normal boot path can restore
//! from it.

use std::path::{Path, PathBuf};

use futures_util::StreamExt;
use nusantara_crypto::Hasher;
use tokio::io::AsyncWriteExt;
use tracing::{info, warn};

use crate::error::ValidatorError;

/// Metadata returned by the `/v1/snapshot/latest` endpoint on entrypoints.
#[derive(Debug, serde::Deserialize)]
struct SnapshotInfo {
    slot: u64,
    #[allow(dead_code)]
    bank_hash: String,
    #[allow(dead_code)]
    account_count: u64,
    #[allow(dead_code)]
    timestamp: i64,
    file_hash: Option<String>,
    #[allow(dead_code)]
    file_size: Option<u64>,
}

/// Attempt to fetch a snapshot from the given entrypoint RPC addresses.
///
/// This function iterates through entrypoints (assumed to expose HTTP RPC on
/// port 8899), queries each for its latest snapshot metadata, selects the
/// snapshot with the highest slot, downloads the binary, verifies the hash,
/// and saves it to `{snapshot_dir}/snapshot-{slot}.bin`.
///
/// Returns `Ok(Some(path))` on success, `Ok(None)` if no entrypoint had a
/// snapshot available, or `Err` on fatal errors.
///
/// This is an async function designed to be called from within the tokio runtime.
#[tracing::instrument(skip(entrypoints, snapshot_dir))]
pub async fn fetch_snapshot_from_entrypoints(
    entrypoints: &[String],
    snapshot_dir: &Path,
) -> Result<Option<PathBuf>, ValidatorError> {
    if entrypoints.is_empty() {
        return Ok(None);
    }

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .build()
        .map_err(|e| ValidatorError::NetworkInit(format!("failed to create HTTP client: {e}")))?;

    // 1. Query each entrypoint for snapshot metadata and pick the highest slot
    let mut best: Option<(SnapshotInfo, String)> = None; // (info, rpc_base_url)

    for ep in entrypoints {
        // Entrypoints are gossip addresses (host:gossip_port).
        // Derive the RPC URL by replacing the port with 8899.
        let rpc_base = entrypoint_to_rpc_url(ep);

        let url = format!("{rpc_base}/v1/snapshot/latest");
        match client.get(&url).send().await {
            Ok(resp) if resp.status().is_success() => match resp.json::<SnapshotInfo>().await {
                Ok(info) => {
                    let dominated = best.as_ref().is_some_and(|(b, _)| b.slot >= info.slot);
                    if !dominated {
                        info!(
                            entrypoint = ep,
                            slot = info.slot,
                            "found snapshot from entrypoint"
                        );
                        best = Some((info, rpc_base));
                    }
                }
                Err(e) => {
                    warn!(entrypoint = ep, error = %e, "failed to parse snapshot info");
                }
            },
            Ok(resp) => {
                warn!(
                    entrypoint = ep,
                    status = %resp.status(),
                    "entrypoint returned non-success for snapshot/latest"
                );
            }
            Err(e) => {
                warn!(entrypoint = ep, error = %e, "failed to query snapshot from entrypoint");
            }
        }
    }

    let Some((info, rpc_base)) = best else {
        info!("no snapshot available from any entrypoint");
        return Ok(None);
    };

    // 2. Download the snapshot binary via streaming to avoid OOM on large snapshots
    let download_url = format!("{rpc_base}/v1/snapshot/download");
    info!(url = %download_url, slot = info.slot, "downloading snapshot");

    let resp = client
        .get(&download_url)
        .send()
        .await
        .map_err(|e| ValidatorError::NetworkInit(format!("snapshot download failed: {e}")))?;

    if !resp.status().is_success() {
        warn!(
            status = %resp.status(),
            "snapshot download returned non-success status"
        );
        return Ok(None);
    }

    // Stream response body to a temp file, hashing incrementally
    tokio::fs::create_dir_all(snapshot_dir)
        .await
        .map_err(ValidatorError::Io)?;

    let tmp_path = snapshot_dir.join(format!("snapshot-{}.bin.tmp", info.slot));
    let mut tmp_file = tokio::fs::File::create(&tmp_path)
        .await
        .map_err(ValidatorError::Io)?;

    let mut hasher = Hasher::default();
    let mut total_bytes: u64 = 0;
    let mut stream = resp.bytes_stream();

    while let Some(chunk_result) = stream.next().await {
        let chunk = chunk_result.map_err(|e| {
            ValidatorError::NetworkInit(format!("failed to read snapshot chunk: {e}"))
        })?;
        hasher.update(&chunk);
        tmp_file.write_all(&chunk).await.map_err(ValidatorError::Io)?;
        total_bytes += chunk.len() as u64;
    }
    tmp_file.flush().await.map_err(ValidatorError::Io)?;
    drop(tmp_file);

    info!(
        size_bytes = total_bytes,
        slot = info.slot,
        "snapshot downloaded"
    );

    // 3. Verify SHA3-512 hash if provided
    if let Some(ref expected_hash) = info.file_hash {
        let computed = hasher.finalize();
        let computed_b64 = computed.to_base64();
        if &computed_b64 != expected_hash {
            warn!(
                expected = expected_hash,
                computed = computed_b64,
                "snapshot hash mismatch — discarding"
            );
            let _ = tokio::fs::remove_file(&tmp_path).await;
            return Ok(None);
        }
        info!("snapshot hash verified");
    } else {
        warn!("entrypoint did not provide file_hash — skipping verification");
    }

    // 4. Atomic rename .tmp → final destination
    let dest = snapshot_dir.join(format!("snapshot-{}.bin", info.slot));
    tokio::fs::rename(&tmp_path, &dest)
        .await
        .map_err(ValidatorError::Io)?;

    info!(
        path = %dest.display(),
        slot = info.slot,
        "snapshot saved to disk"
    );

    metrics::counter!("nusantara_snapshots_fetched").increment(1);

    Ok(Some(dest))
}

/// Derive an HTTP RPC base URL from an entrypoint gossip address.
///
/// Entrypoints are specified as `host:port` for gossip. The RPC server runs
/// on port 8899 by convention. This strips the gossip port and replaces it.
fn entrypoint_to_rpc_url(entrypoint: &str) -> String {
    // Handle [IPv6]:port format
    if let Some(bracket_end) = entrypoint.rfind(']') {
        let host_part = &entrypoint[..=bracket_end];
        return format!("http://{host_part}:8899");
    }

    // Handle host:port (IPv4 or hostname)
    if let Some(colon_pos) = entrypoint.rfind(':') {
        let host = &entrypoint[..colon_pos];
        return format!("http://{host}:8899");
    }

    // No port specified — use as-is
    format!("http://{entrypoint}:8899")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entrypoint_url_ipv4() {
        assert_eq!(
            entrypoint_to_rpc_url("192.168.1.1:8000"),
            "http://192.168.1.1:8899"
        );
    }

    #[test]
    fn entrypoint_url_hostname() {
        assert_eq!(
            entrypoint_to_rpc_url("validator-1:8000"),
            "http://validator-1:8899"
        );
    }

    #[test]
    fn entrypoint_url_no_port() {
        assert_eq!(
            entrypoint_to_rpc_url("validator-1"),
            "http://validator-1:8899"
        );
    }

    #[test]
    fn entrypoint_url_ipv6() {
        assert_eq!(
            entrypoint_to_rpc_url("[::1]:8000"),
            "http://[::1]:8899"
        );
    }
}
