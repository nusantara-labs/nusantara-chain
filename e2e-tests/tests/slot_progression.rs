mod common;

use nusantara_e2e_tests::types::SlotResponse;

#[tokio::test]
async fn slots_advance_over_time() {
    skip_unless_e2e!();
    let client = common::make_client();
    common::wait_ready(&client).await;

    let before: SlotResponse = client.get("/v1/slot").await.expect("get slot");
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    let after: SlotResponse = client.get("/v1/slot").await.expect("get slot");

    assert!(
        after.slot > before.slot,
        "slot should advance: {} -> {}",
        before.slot,
        after.slot
    );
}

#[tokio::test]
async fn roots_finalize() {
    skip_unless_e2e!();
    let client = common::make_client();
    common::wait_ready(&client).await;

    let resp: SlotResponse = client.get("/v1/slot").await.expect("get slot");

    let root = resp.latest_root.expect("should have a root");
    let slot = resp.slot;

    // Root should not be too far behind current slot
    assert!(
        slot - root < 100,
        "root {root} too far behind slot {slot}"
    );
}
