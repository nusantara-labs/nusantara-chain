mod common;

use std::time::Duration;

use nusantara_core::nusa_to_lamports;
use nusantara_crypto::Keypair;
use nusantara_e2e_tests::tx_builder;
use nusantara_e2e_tests::types::AccountResponse;

#[tokio::test]
async fn airdrop_increases_balance() {
    skip_unless_e2e!();
    let client = common::make_client();
    common::wait_ready(&client).await;

    let kp = Keypair::generate();
    let address = kp.address();
    let address_b64 = address.to_base64();
    let lamports = nusa_to_lamports(0.5);

    // Airdrop 0.5 NUSA
    let sig = tx_builder::airdrop(&client, &address, lamports)
        .await
        .expect("airdrop");
    tx_builder::wait_for_confirmation(&client, &sig, Duration::from_secs(30))
        .await
        .expect("confirm");

    // Wait for state to root
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Verify balance
    let path = format!("/v1/account/{address_b64}");
    let account: AccountResponse = client.get(&path).await.expect("get account");
    assert!(
        account.lamports >= lamports,
        "expected >= {lamports} lamports, got {}",
        account.lamports
    );
}

#[tokio::test]
async fn airdrop_over_10_nusa_rejected() {
    skip_unless_e2e!();
    let client = common::make_client();
    common::wait_ready(&client).await;

    let kp = Keypair::generate();
    let address = kp.address();
    let lamports = nusa_to_lamports(11.0); // Over the 10 NUSA max

    let result = tx_builder::airdrop(&client, &address, lamports).await;
    assert!(result.is_err(), "airdrop > 10 NUSA should be rejected");
}
