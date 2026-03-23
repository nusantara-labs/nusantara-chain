mod common;

use nusantara_e2e_tests::types::{EpochInfoResponse, ValidatorsResponse};

#[tokio::test]
async fn three_validators_with_stake() {
    skip_unless_e2e!();
    let client = common::make_client();
    common::wait_ready(&client).await;

    let resp: ValidatorsResponse = client.get("/v1/validators").await.expect("get validators");

    assert_eq!(
        resp.validators.len(),
        3,
        "expected 3 validators, got {}",
        resp.validators.len()
    );

    for v in &resp.validators {
        assert!(
            v.active_stake > 0,
            "validator {} has zero stake",
            v.identity
        );
    }

    assert!(
        resp.total_active_stake > 0,
        "total_active_stake should be > 0"
    );
}

#[tokio::test]
async fn epoch_info_slots_in_epoch() {
    skip_unless_e2e!();
    let client = common::make_client();
    common::wait_ready(&client).await;

    let resp: EpochInfoResponse = client.get("/v1/epoch-info").await.expect("get epoch info");

    assert_eq!(
        resp.slots_in_epoch, 432_000,
        "expected 432000 slots per epoch"
    );
    assert!(resp.absolute_slot > 0, "absolute_slot should be > 0");
}
