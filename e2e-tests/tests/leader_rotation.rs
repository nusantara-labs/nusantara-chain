mod common;

use std::collections::HashSet;

use nusantara_e2e_tests::types::LeaderScheduleResponse;

#[tokio::test]
async fn leader_schedule_has_entries() {
    skip_unless_e2e!();
    let client = common::make_client();
    common::wait_ready(&client).await;

    let resp: LeaderScheduleResponse = client
        .get("/v1/leader-schedule")
        .await
        .expect("get leader schedule");

    assert!(
        !resp.schedule.is_empty(),
        "leader schedule should have entries"
    );
}

#[tokio::test]
async fn multiple_validators_produce_blocks() {
    skip_unless_e2e!();
    let client = common::make_client();
    common::wait_ready(&client).await;

    let resp: LeaderScheduleResponse = client
        .get("/v1/leader-schedule")
        .await
        .expect("get leader schedule");

    let leaders: HashSet<&str> = resp.schedule.iter().map(|e| e.leader.as_str()).collect();

    assert!(
        leaders.len() >= 2,
        "expected at least 2 distinct leaders, got {}",
        leaders.len()
    );
}
