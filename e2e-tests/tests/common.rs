#![allow(dead_code)]

use std::time::Duration;

use nusantara_e2e_tests::client::{ClientConfig, NusantaraClient};
use nusantara_e2e_tests::cluster::{self, ClusterState};

/// Default RPC URL via nginx proxy (random load-balancing across 3 validators).
pub const PROXY_URL: &str = "http://localhost:8080";

/// Direct RPC URLs for the 3-validator Docker cluster.
pub const DEFAULT_URLS: &[&str] = &[
    "http://localhost:8899",
    "http://localhost:8900",
    "http://localhost:8901",
];

/// Skip test if `NUSANTARA_E2E` env var is not set.
#[macro_export]
macro_rules! skip_unless_e2e {
    () => {
        if std::env::var("NUSANTARA_E2E").is_err() {
            eprintln!("skipping e2e test (set NUSANTARA_E2E=1 to run)");
            return;
        }
    };
}

/// Build a client via the nginx proxy (random load-balancing).
/// Use this for tests that only need single-node operations.
pub fn make_client() -> NusantaraClient {
    NusantaraClient::new(
        vec![PROXY_URL.to_string()],
        ClientConfig {
            timeout: Duration::from_secs(15),
            max_retries: 3,
            retry_backoff: Duration::from_millis(500),
            ..ClientConfig::default()
        },
    )
}

/// Build a client with direct access to all 3 validator nodes.
/// Use this for tests that need `get_all()` or `get_from()` multi-node queries.
pub fn make_multi_node_client() -> NusantaraClient {
    let urls: Vec<String> = DEFAULT_URLS.iter().map(|u| (*u).to_string()).collect();
    NusantaraClient::new(
        urls,
        ClientConfig {
            timeout: Duration::from_secs(15),
            max_retries: 3,
            retry_backoff: Duration::from_millis(500),
            ..ClientConfig::default()
        },
    )
}

/// Wait for the cluster to become ready (1 node via proxy, 60s timeout).
pub async fn wait_ready(client: &NusantaraClient) -> ClusterState {
    cluster::wait_for_cluster_ready(client, 1, Duration::from_secs(60))
        .await
        .expect("cluster should be ready")
}

/// Wait for the cluster to become ready (3 nodes via direct URLs, 60s timeout).
pub async fn wait_ready_all(client: &NusantaraClient) -> ClusterState {
    cluster::wait_for_cluster_ready(client, 3, Duration::from_secs(60))
        .await
        .expect("cluster should be ready")
}
