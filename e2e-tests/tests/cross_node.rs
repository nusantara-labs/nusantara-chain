mod common;

use nusantara_e2e_tests::types::{BlockResponse, SlotResponse};

#[tokio::test]
async fn same_block_hash_across_nodes() {
    skip_unless_e2e!();
    let client = common::make_multi_node_client();
    common::wait_ready_all(&client).await;

    // Get a finalized slot from node 0
    let slot_resp: SlotResponse = client.get("/v1/slot").await.expect("get slot");
    let root = slot_resp.latest_root.expect("should have a root");

    // Fetch the block at that root from all nodes
    let path = format!("/v1/block/{root}");
    let results = client.get_all::<BlockResponse>(&path).await;

    let blocks: Vec<BlockResponse> = results
        .into_iter()
        .enumerate()
        .map(|(i, r)| r.unwrap_or_else(|e| panic!("node {i} failed to return block {root}: {e}")))
        .collect();

    assert_eq!(blocks.len(), 3);

    let hash0 = &blocks[0].block_hash;
    let merkle0 = &blocks[0].merkle_root;

    for (i, block) in blocks.iter().enumerate().skip(1) {
        assert_eq!(
            &block.block_hash, hash0,
            "node {i} block_hash differs from node 0"
        );
        assert_eq!(
            &block.merkle_root, merkle0,
            "node {i} merkle_root differs from node 0"
        );
    }
}
