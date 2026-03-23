mod common;

use std::time::Duration;

use nusantara_core::nusa_to_lamports;
use nusantara_crypto::Keypair;
use nusantara_e2e_tests::client::NusantaraClient;
use nusantara_e2e_tests::tx_builder;
use nusantara_e2e_tests::types::SendTransactionRequest;

#[tokio::test]
async fn transaction_lifecycle() {
    skip_unless_e2e!();
    let client = common::make_client();
    common::wait_ready(&client).await;

    // Fund an account
    let kp = Keypair::generate();
    let receiver = Keypair::generate();
    let addr = kp.address();
    let fund = nusa_to_lamports(1.0);

    let sig = tx_builder::airdrop(&client, &addr, fund)
        .await
        .expect("airdrop");
    tx_builder::wait_for_confirmation(&client, &sig, Duration::from_secs(30))
        .await
        .expect("confirm airdrop");

    // Wait for state to settle across slots
    tokio::time::sleep(Duration::from_secs(1)).await;

    // Send transfer and poll for confirmation
    let transfer_lamports = nusa_to_lamports(0.1);
    let sig =
        tx_builder::send_transfer(&client, &kp, &receiver.address(), transfer_lamports)
            .await
            .expect("send transfer");

    let status = tx_builder::wait_for_confirmation(&client, &sig, Duration::from_secs(30))
        .await
        .expect("confirm transfer");

    assert_eq!(status.status, "success", "tx should succeed");
    assert!(status.fee > 0, "fee should be > 0");
    assert!(status.slot > 0, "slot should be > 0");
}

#[tokio::test]
async fn invalid_transaction_rejected() {
    skip_unless_e2e!();
    let client = common::make_client();
    common::wait_ready(&client).await;

    let result = send_garbage(&client).await;
    assert!(result.is_err(), "garbage tx should be rejected");
}

async fn send_garbage(
    client: &NusantaraClient,
) -> Result<(), nusantara_e2e_tests::error::E2eError> {
    use nusantara_e2e_tests::types::SendTransactionResponse;

    let _resp: SendTransactionResponse = client
        .post(
            "/v1/transaction/send",
            &SendTransactionRequest {
                transaction: "not-a-valid-transaction".to_string(),
            },
        )
        .await?;
    Ok(())
}
