use std::time::{Duration, Instant};

use tracing::{info, warn};

use crate::client::NusantaraClient;
use crate::error::E2eError;
use crate::types::HealthResponse;

#[derive(Debug, Clone)]
pub struct ClusterState {
    pub nodes: Vec<HealthResponse>,
    pub epoch: u64,
}

/// Poll `/v1/health` on all nodes until the cluster is ready.
///
/// Ready means:
/// - All nodes return HTTP 200
/// - All nodes report the same epoch
/// - `peer_count >= expected_nodes - 1` on every node
/// - Slots are advancing (slot > 0)
pub async fn wait_for_cluster_ready(
    client: &NusantaraClient,
    expected_nodes: usize,
    timeout: Duration,
) -> Result<ClusterState, E2eError> {
    let start = Instant::now();
    let poll_interval = Duration::from_secs(2);

    loop {
        if start.elapsed() > timeout {
            return Err(E2eError::ClusterNotReady(format!(
                "timed out after {timeout:?} waiting for {expected_nodes} nodes"
            )));
        }

        let results = client.get_all::<HealthResponse>("/v1/health").await;

        // Check all succeeded
        let mut nodes = Vec::with_capacity(expected_nodes);
        let mut all_ok = true;
        for (i, result) in results.into_iter().enumerate() {
            match result {
                Ok(health) => nodes.push(health),
                Err(e) => {
                    warn!(node = i, %e, "node not ready");
                    all_ok = false;
                    break;
                }
            }
        }

        if !all_ok || nodes.len() < expected_nodes {
            tokio::time::sleep(poll_interval).await;
            continue;
        }

        // Check all nodes report ok status
        if nodes.iter().any(|n| n.status != "ok") {
            let statuses: Vec<_> = nodes.iter().map(|n| n.status.as_str()).collect();
            warn!(?statuses, "not all nodes report ok");
            tokio::time::sleep(poll_interval).await;
            continue;
        }

        // Check peer count
        let min_peers = expected_nodes.saturating_sub(1);
        if nodes.iter().any(|n| n.peer_count < min_peers) {
            let peers: Vec<_> = nodes.iter().map(|n| n.peer_count).collect();
            warn!(?peers, min_peers, "insufficient peers");
            tokio::time::sleep(poll_interval).await;
            continue;
        }

        // Check same epoch
        let epoch = nodes[0].epoch;
        if nodes.iter().any(|n| n.epoch != epoch) {
            let epochs: Vec<_> = nodes.iter().map(|n| n.epoch).collect();
            warn!(?epochs, "epoch mismatch across nodes");
            tokio::time::sleep(poll_interval).await;
            continue;
        }

        // Check slots advancing
        if nodes.iter().any(|n| n.slot == 0) {
            warn!("some nodes still at slot 0");
            tokio::time::sleep(poll_interval).await;
            continue;
        }

        info!(
            epoch,
            node_count = nodes.len(),
            slots = ?nodes.iter().map(|n| n.slot).collect::<Vec<_>>(),
            "cluster ready"
        );
        return Ok(ClusterState { nodes, epoch });
    }
}
