//! Snapshot fetcher for validator bootstrap.
//!
//! Queries entrypoint RPC servers for the latest available snapshot,
//! downloads the binary file, verifies its SHA3-512 hash, and saves it
//! to the local snapshots directory so the normal boot path can restore
//! from it.

use std::cmp::Reverse;
use std::path::{Path, PathBuf};

use futures_util::StreamExt;
use nusantara_crypto::Hasher;
use tokio::io::AsyncWriteExt;
use tracing::{info, warn};

use crate::error::ValidatorError;

/// Hard cap on streaming download size regardless of reported file_size.
/// 64 GiB is generous enough for any realistic snapshot while bounding
/// memory/disk abuse from a malicious or misconfigured entrypoint.
const MAX_SNAPSHOT_SIZE: u64 = 64 * 1024 * 1024 * 1024;

/// Metadata returned by the `/v1/snapshot/latest` endpoint on entrypoints.
#[derive(Debug, serde::Deserialize)]
struct SnapshotInfo {
    slot: u64,
    file_hash: Option<String>,
    file_size: Option<u64>,
}

/// RAII guard that deletes the tmp file on drop unless `commit()` is called.
struct TmpFileGuard {
    path: Option<PathBuf>,
}

impl TmpFileGuard {
    fn new(path: PathBuf) -> Self {
        Self { path: Some(path) }
    }

    /// Transfer ownership: caller is responsible for the file from here on.
    fn commit(mut self) -> PathBuf {
        self.path.take().expect("commit called twice")
    }
}

impl Drop for TmpFileGuard {
    fn drop(&mut self) {
        if let Some(ref path) = self.path {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Attempt to fetch a snapshot from the given entrypoint RPC addresses.
///
/// This function iterates through entrypoints (assumed to expose HTTP RPC on
/// port 8899), queries each for its latest snapshot metadata, selects the
/// snapshot with the highest slot (sorted descending), then tries each in order
/// until one succeeds. Both `file_hash` and `file_size` must be present in the
/// metadata — snapshots missing either field are rejected.
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

    // Short timeout for metadata GETs; long timeout for binary downloads.
    let meta_client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| ValidatorError::NetworkInit(format!("failed to create HTTP client: {e}")))?;
    let download_client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .build()
        .map_err(|e| ValidatorError::NetworkInit(format!("failed to create HTTP client: {e}")))?;

    // 1. Query each entrypoint for snapshot metadata, collect all valid candidates.
    let mut candidates: Vec<(SnapshotInfo, String)> = Vec::new(); // (info, rpc_base_url)

    for ep in entrypoints {
        // Entrypoints are gossip addresses (host:gossip_port).
        // Derive the RPC URL by replacing the port with 8899.
        let rpc_base = entrypoint_to_rpc_url(ep);

        let url = format!("{rpc_base}/v1/snapshot/latest");
        match meta_client.get(&url).send().await {
            Ok(resp) if resp.status().is_success() => match resp.json::<SnapshotInfo>().await {
                Ok(info) => {
                    // Both file_hash and file_size must be present; reject snapshots
                    // that omit either — we cannot safely download or verify them.
                    if info.file_hash.is_none() || info.file_size.is_none() {
                        warn!(
                            entrypoint = ep,
                            slot = info.slot,
                            "snapshot metadata missing file_hash or file_size — rejecting"
                        );
                        continue;
                    }
                    info!(
                        entrypoint = ep,
                        slot = info.slot,
                        "found snapshot from entrypoint"
                    );
                    candidates.push((info, rpc_base));
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

    if candidates.is_empty() {
        info!("no snapshot available from any entrypoint");
        return Ok(None);
    }

    // Sort descending by slot — try the freshest snapshot first.
    candidates.sort_by_key(|c| Reverse(c.0.slot));

    tokio::fs::create_dir_all(snapshot_dir)
        .await
        .map_err(ValidatorError::Io)?;

    // 2. Try each candidate in order; move on if download/verify fails.
    for (info, rpc_base) in candidates {
        // Both fields were verified present during collection.
        let expected_hash = info.file_hash.as_deref().expect("checked above");
        let file_size = info.file_size.expect("checked above");
        // Apply absolute hard cap in case server reports an absurd file_size.
        let size_cap = file_size.min(MAX_SNAPSHOT_SIZE);

        let download_url = format!("{rpc_base}/v1/snapshot/download");
        info!(url = %download_url, slot = info.slot, "downloading snapshot");

        let resp = match download_client.get(&download_url).send().await {
            Ok(r) if r.status().is_success() => r,
            Ok(r) => {
                warn!(
                    status = %r.status(),
                    slot = info.slot,
                    "snapshot download returned non-success status, trying next"
                );
                continue;
            }
            Err(e) => {
                warn!(error = %e, slot = info.slot, "snapshot download failed, trying next");
                continue;
            }
        };

        // Stream response body to a temp file, hashing incrementally.
        // TmpFileGuard removes the file on drop if we bail out early.
        let tmp_path = snapshot_dir.join(format!("snapshot-{}.bin.tmp", info.slot));
        let tmp_guard = TmpFileGuard::new(tmp_path.clone());

        let mut tmp_file = match tokio::fs::File::create(&tmp_path).await {
            Ok(f) => f,
            Err(e) => {
                warn!(error = %e, "failed to create tmp snapshot file, trying next");
                continue;
            }
        };

        let mut hasher = Hasher::default();
        let mut total_bytes: u64 = 0;
        let mut stream = resp.bytes_stream();
        let mut size_exceeded = false;

        while let Some(chunk_result) = stream.next().await {
            let chunk = match chunk_result {
                Ok(c) => c,
                Err(e) => {
                    warn!(error = %e, "failed to read snapshot chunk, trying next");
                    size_exceeded = true; // reuse flag to break out and skip
                    break;
                }
            };
            hasher.update(&chunk);
            if let Err(e) = tmp_file.write_all(&chunk).await {
                warn!(error = %e, "failed to write snapshot chunk, trying next");
                size_exceeded = true;
                break;
            }
            total_bytes += chunk.len() as u64;
            if total_bytes > size_cap {
                warn!(
                    total_bytes,
                    size_cap,
                    slot = info.slot,
                    "snapshot download exceeded size cap — aborting"
                );
                size_exceeded = true;
                break;
            }
        }

        // tmp_guard drops here on `continue`, removing the file.
        if size_exceeded {
            drop(tmp_guard);
            continue;
        }

        if let Err(e) = tmp_file.flush().await {
            warn!(error = %e, "failed to flush snapshot tmp file, trying next");
            drop(tmp_guard);
            continue;
        }
        drop(tmp_file);

        info!(
            size_bytes = total_bytes,
            slot = info.slot,
            "snapshot downloaded"
        );

        // 3. Verify SHA3-512 hash — mandatory (both fields required at collection time).
        let computed = hasher.finalize();
        let computed_b64 = computed.to_base64();
        if computed_b64 != expected_hash {
            warn!(
                expected = expected_hash,
                computed = computed_b64,
                slot = info.slot,
                "snapshot hash mismatch — discarding, trying next"
            );
            drop(tmp_guard);
            continue;
        }
        info!(slot = info.slot, "snapshot hash verified");

        // 4. Atomic rename .tmp → final destination.
        let dest = snapshot_dir.join(format!("snapshot-{}.bin", info.slot));
        let committed_tmp = tmp_guard.commit();
        if let Err(e) = tokio::fs::rename(&committed_tmp, &dest).await {
            warn!(error = %e, "failed to rename snapshot tmp file, trying next");
            let _ = tokio::fs::remove_file(&committed_tmp).await;
            continue;
        }

        info!(
            path = %dest.display(),
            slot = info.slot,
            "snapshot saved to disk"
        );

        metrics::counter!("nusantara_snapshots_fetched").increment(1);

        return Ok(Some(dest));
    }

    Ok(None)
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
        assert_eq!(entrypoint_to_rpc_url("[::1]:8000"), "http://[::1]:8899");
    }

    #[test]
    fn tmp_file_guard_deletes_on_drop() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.tmp");
        std::fs::write(&path, b"data").unwrap();
        assert!(path.exists());
        let guard = TmpFileGuard::new(path.clone());
        drop(guard);
        assert!(!path.exists());
    }

    #[test]
    fn tmp_file_guard_keeps_on_commit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.tmp");
        std::fs::write(&path, b"data").unwrap();
        assert!(path.exists());
        let guard = TmpFileGuard::new(path.clone());
        let returned = guard.commit();
        assert_eq!(returned, path);
        assert!(path.exists());
    }
}
