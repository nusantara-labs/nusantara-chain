use std::sync::Arc;

use nusantara_crypto::Keypair;
use tokio::task::JoinSet;
use tracing::{info, warn};

use crate::client::NusantaraClient;
use crate::error::E2eError;
use crate::tx_builder;

/// Fund multiple accounts via sequential airdrops.
///
/// Uses the `airdrop-and-confirm` endpoint for sub-slot confirmation latency.
/// The faucet has a max of 10 NUSA per request.
pub async fn fund_accounts(
    client: &NusantaraClient,
    keypairs: &[Keypair],
    lamports_each: u64,
) -> Result<(), E2eError> {
    let confirm_timeout_ms = 30_000;

    for (i, kp) in keypairs.iter().enumerate() {
        let address = kp.address();
        let resp =
            tx_builder::airdrop_and_confirm(client, &address, lamports_each, confirm_timeout_ms)
                .await?;
        if !resp.status.starts_with("success") {
            return Err(E2eError::Other(format!(
                "airdrop for account {i} failed: {}",
                resp.status
            )));
        }
        info!(
            account = i,
            address = %address.to_base64(),
            lamports = lamports_each,
            confirmation_time_ms = resp.confirmation_time_ms,
            "funded account"
        );
    }

    Ok(())
}

/// Fund multiple accounts via parallel batched airdrops.
///
/// Splits keypairs into chunks of `batch_size`, fires up to `concurrency`
/// airdrop-and-confirm requests concurrently within each chunk.
/// Each request confirms in a single HTTP round-trip via the pubsub channel.
pub async fn fund_accounts_parallel(
    client: Arc<NusantaraClient>,
    keypairs: &[Keypair],
    lamports_each: u64,
    batch_size: usize,
    concurrency: usize,
) -> Result<(), E2eError> {
    let confirm_timeout_ms: u64 = 30_000;

    let total = keypairs.len();
    let chunks: Vec<&[Keypair]> = keypairs.chunks(batch_size).collect();

    for (chunk_idx, chunk) in chunks.iter().enumerate() {
        let chunk_start = chunk_idx * batch_size;
        info!(
            chunk = chunk_idx,
            accounts = format!("{}-{}", chunk_start, chunk_start + chunk.len() - 1),
            total,
            "funding chunk"
        );

        let addresses: Vec<_> = chunk.iter().map(|kp| kp.address()).collect();

        let semaphore = Arc::new(tokio::sync::Semaphore::new(concurrency));
        let mut join_set = JoinSet::new();

        for (i, address) in addresses.iter().enumerate() {
            let client = client.clone();
            let address = *address;
            let sem = semaphore.clone();
            let account_idx = chunk_start + i;

            join_set.spawn(async move {
                let _permit = sem.acquire().await.expect("semaphore closed");
                let resp = tx_builder::airdrop_and_confirm(
                    &client,
                    &address,
                    lamports_each,
                    confirm_timeout_ms,
                )
                .await?;
                Ok::<(usize, String, String), E2eError>((
                    account_idx,
                    resp.signature,
                    resp.status,
                ))
            });
        }

        let mut confirmed = 0usize;
        let mut failed = 0usize;

        while let Some(result) = join_set.join_next().await {
            match result {
                Ok(Ok((_idx, _sig, status))) => {
                    if status.starts_with("success") {
                        confirmed += 1;
                    } else {
                        failed += 1;
                        warn!(status, "airdrop completed with non-success status");
                    }
                }
                Ok(Err(e)) => {
                    failed += 1;
                    warn!(%e, "airdrop-and-confirm failed");
                }
                Err(e) => {
                    failed += 1;
                    warn!(%e, "airdrop task panicked");
                }
            }
        }

        info!(
            chunk = chunk_idx,
            confirmed,
            failed,
            "chunk funding complete"
        );
    }

    info!(total, "all accounts funded");
    Ok(())
}
