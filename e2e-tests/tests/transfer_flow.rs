mod common;

use std::time::Duration;

use nusantara_core::nusa_to_lamports;
use nusantara_crypto::Keypair;
use nusantara_e2e_tests::tx_builder;
use nusantara_e2e_tests::types::AccountResponse;

#[tokio::test]
async fn transfer_moves_funds() {
    skip_unless_e2e!();
    let client = common::make_client();
    common::wait_ready(&client).await;

    let sender = Keypair::generate();
    let receiver = Keypair::generate();
    let sender_addr = sender.address();
    let receiver_addr = receiver.address();

    // Fund sender with 1 NUSA
    let fund_lamports = nusa_to_lamports(1.0);
    let sig = tx_builder::airdrop(&client, &sender_addr, fund_lamports)
        .await
        .expect("airdrop");
    tx_builder::wait_for_confirmation(&client, &sig, Duration::from_secs(30))
        .await
        .expect("confirm airdrop");

    // Wait for state to settle across slots
    tokio::time::sleep(Duration::from_secs(1)).await;

    // Transfer 0.1 NUSA to receiver
    let transfer_lamports = nusa_to_lamports(0.1);
    let sig = tx_builder::send_transfer(&client, &sender, &receiver_addr, transfer_lamports)
        .await
        .expect("transfer");
    tx_builder::wait_for_confirmation(&client, &sig, Duration::from_secs(30))
        .await
        .expect("confirm transfer");

    // Wait for state to root
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Verify receiver balance
    let path = format!("/v1/account/{}", receiver_addr.to_base64());
    let account: AccountResponse = client.get(&path).await.expect("get receiver");
    assert_eq!(
        account.lamports, transfer_lamports,
        "receiver should have exactly {transfer_lamports} lamports"
    );

    // Verify sender balance decreased (balance = funded - transferred - fee)
    let path = format!("/v1/account/{}", sender_addr.to_base64());
    let sender_account: AccountResponse = client.get(&path).await.expect("get sender");
    assert!(
        sender_account.lamports < fund_lamports,
        "sender balance should decrease"
    );
    assert!(
        sender_account.lamports >= fund_lamports - transfer_lamports - nusa_to_lamports(0.1),
        "sender balance should not decrease by more than transfer + reasonable fee"
    );
}
