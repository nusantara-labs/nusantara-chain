mod common;

use std::collections::HashSet;

use nusantara_e2e_tests::types::HealthResponse;

#[tokio::test]
async fn all_nodes_healthy() {
    skip_unless_e2e!();
    let client = common::make_multi_node_client();
    common::wait_ready_all(&client).await;

    let results = client.get_all::<HealthResponse>("/v1/health").await;
    assert_eq!(results.len(), 3, "expected 3 node responses");

    for (i, result) in results.into_iter().enumerate() {
        let health = result.unwrap_or_else(|e| panic!("node {i} unhealthy: {e}"));
        assert_eq!(health.status, "ok", "node {i} status not ok");
        assert!(health.peer_count >= 2, "node {i} has < 2 peers");
        assert!(health.slot > 0, "node {i} at slot 0");
        assert!(
            health.total_active_stake > 0,
            "node {i} has zero active stake"
        );
    }
}

#[tokio::test]
async fn unique_identities() {
    skip_unless_e2e!();
    let client = common::make_multi_node_client();
    common::wait_ready_all(&client).await;

    let results = client.get_all::<HealthResponse>("/v1/health").await;
    let identities: HashSet<String> = results
        .into_iter()
        .map(|r| r.expect("healthy").identity)
        .collect();

    assert_eq!(identities.len(), 3, "expected 3 unique identities");
}

#[tokio::test]
async fn same_epoch_across_nodes() {
    skip_unless_e2e!();
    let client = common::make_multi_node_client();
    let state = common::wait_ready_all(&client).await;

    let epochs: HashSet<u64> = state.nodes.iter().map(|n| n.epoch).collect();
    assert_eq!(epochs.len(), 1, "all nodes should be in the same epoch");
}
