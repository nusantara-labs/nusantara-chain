use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use nusantara_core::{Message, Transaction};
use nusantara_crypto::{Hash, Keypair};
use tokio::task::JoinSet;
use tracing::{debug, warn};

use crate::client::NusantaraClient;
use crate::error::E2eError;
use crate::types::{
    AirdropAndConfirmRequest, AirdropAndConfirmResponse, AirdropRequest, AirdropResponse,
    BlockhashResponse, SendAndConfirmRequest, SendAndConfirmResponse, SendTransactionRequest,
    SendTransactionResponse, TransactionStatusResponse,
};

/// Fetch the latest blockhash from the primary node.
pub async fn fetch_blockhash(client: &NusantaraClient) -> Result<Hash, E2eError> {
    let resp: BlockhashResponse = client.get("/v1/blockhash").await?;
    Hash::from_base64(&resp.blockhash).map_err(|e| E2eError::Crypto(format!("invalid blockhash: {e}")))
}

/// Build a signed transfer transaction and return it as a base64-encoded string.
pub fn build_transfer(
    keypair: &Keypair,
    to: &Hash,
    lamports: u64,
    blockhash: &Hash,
) -> Result<String, E2eError> {
    let from = keypair.address();
    let ix = nusantara_system_program::transfer(&from, to, lamports);
    let mut msg = Message::new(&[ix], &from)
        .map_err(|e| E2eError::Other(format!("failed to build message: {e}")))?;
    msg.recent_blockhash = *blockhash;

    let mut tx = Transaction::new(msg);
    tx.sign(&[keypair]);

    let bytes =
        borsh::to_vec(&tx).map_err(|e| E2eError::Serialization(e.to_string()))?;
    Ok(URL_SAFE_NO_PAD.encode(&bytes))
}

/// Build a signed transfer with a unique nonce to prevent duplicate tx hashes.
/// The nonce is encoded as a `SetComputeUnitPrice` instruction prepended to the message.
pub fn build_transfer_with_nonce(
    keypair: &Keypair,
    to: &Hash,
    lamports: u64,
    blockhash: &Hash,
    nonce: u64,
) -> Result<String, E2eError> {
    let from = keypair.address();
    let nonce_ix = nusantara_compute_budget_program::set_compute_unit_price(nonce);
    let transfer_ix = nusantara_system_program::transfer(&from, to, lamports);
    let mut msg = Message::new(&[nonce_ix, transfer_ix], &from)
        .map_err(|e| E2eError::Other(format!("failed to build message: {e}")))?;
    msg.recent_blockhash = *blockhash;

    let mut tx = Transaction::new(msg);
    tx.sign(&[keypair]);

    let bytes =
        borsh::to_vec(&tx).map_err(|e| E2eError::Serialization(e.to_string()))?;
    Ok(URL_SAFE_NO_PAD.encode(&bytes))
}

/// Build a signed transfer with a unique nonce and return both the encoded tx
/// and the pre-computed signature (base64 of tx hash). Allows the caller to
/// know the signature before submitting.
pub fn build_transfer_with_nonce_and_sig(
    keypair: &Keypair,
    to: &Hash,
    lamports: u64,
    blockhash: &Hash,
    nonce: u64,
) -> Result<(String, String), E2eError> {
    let from = keypair.address();
    let nonce_ix = nusantara_compute_budget_program::set_compute_unit_price(nonce);
    let transfer_ix = nusantara_system_program::transfer(&from, to, lamports);
    let mut msg = Message::new(&[nonce_ix, transfer_ix], &from)
        .map_err(|e| E2eError::Other(format!("failed to build message: {e}")))?;
    msg.recent_blockhash = *blockhash;

    let mut tx = Transaction::new(msg);
    tx.sign(&[keypair]);

    let signature = tx.hash().to_base64();
    let bytes =
        borsh::to_vec(&tx).map_err(|e| E2eError::Serialization(e.to_string()))?;
    let encoded = URL_SAFE_NO_PAD.encode(&bytes);
    Ok((encoded, signature))
}

/// Send a transfer: fetch blockhash, build, sign, submit. Returns signature.
pub async fn send_transfer(
    client: &NusantaraClient,
    keypair: &Keypair,
    to: &Hash,
    lamports: u64,
) -> Result<String, E2eError> {
    let blockhash = fetch_blockhash(client).await?;
    let encoded = build_transfer(keypair, to, lamports, &blockhash)?;

    let resp: SendTransactionResponse = client
        .post(
            "/v1/transaction/send",
            &SendTransactionRequest {
                transaction: encoded,
            },
        )
        .await?;

    debug!(signature = %resp.signature, "transfer sent");
    Ok(resp.signature)
}

/// Send a transfer using the send-and-confirm endpoint for sub-slot confirmation.
/// Returns the confirmed response directly without polling.
pub async fn send_transfer_and_confirm(
    client: &NusantaraClient,
    keypair: &Keypair,
    to: &Hash,
    lamports: u64,
    timeout_ms: u64,
) -> Result<SendAndConfirmResponse, E2eError> {
    let blockhash = fetch_blockhash(client).await?;
    let encoded = build_transfer(keypair, to, lamports, &blockhash)?;

    let resp: SendAndConfirmResponse = client
        .post(
            "/v1/transaction/send-and-confirm",
            &SendAndConfirmRequest {
                transaction: encoded,
                timeout_ms,
            },
        )
        .await?;

    debug!(
        signature = %resp.signature,
        slot = resp.slot,
        confirmation_time_ms = resp.confirmation_time_ms,
        "transfer confirmed"
    );
    Ok(resp)
}

/// Request an airdrop for the given address. Returns signature.
pub async fn airdrop(
    client: &NusantaraClient,
    address: &Hash,
    lamports: u64,
) -> Result<String, E2eError> {
    let resp: AirdropResponse = client
        .post(
            "/v1/airdrop",
            &AirdropRequest {
                address: address.to_base64(),
                lamports,
            },
        )
        .await?;

    debug!(signature = %resp.signature, "airdrop requested");
    Ok(resp.signature)
}

/// Request an airdrop and wait for confirmation in a single request.
/// Returns the confirmed response directly without polling.
pub async fn airdrop_and_confirm(
    client: &NusantaraClient,
    address: &Hash,
    lamports: u64,
    timeout_ms: u64,
) -> Result<AirdropAndConfirmResponse, E2eError> {
    let resp: AirdropAndConfirmResponse = client
        .post(
            "/v1/airdrop-and-confirm",
            &AirdropAndConfirmRequest {
                address: address.to_base64(),
                lamports,
                timeout_ms,
            },
        )
        .await?;

    debug!(
        signature = %resp.signature,
        slot = resp.slot,
        confirmation_time_ms = resp.confirmation_time_ms,
        "airdrop confirmed"
    );
    Ok(resp)
}

/// Poll `/v1/transaction/{sig}` until confirmed or timeout.
///
/// Uses exponential backoff starting at 400ms (aligned to slot time), doubling
/// up to 1s. Recognises the `"received"` status (transaction in mempool, not
/// yet confirmed). Prefer `send_transfer_and_confirm` for sub-slot latency.
pub async fn wait_for_confirmation(
    client: &NusantaraClient,
    signature: &str,
    timeout: Duration,
) -> Result<TransactionStatusResponse, E2eError> {
    let start = Instant::now();
    let mut poll_interval = Duration::from_millis(100);
    let max_interval = Duration::from_millis(500);

    loop {
        if start.elapsed() > timeout {
            return Err(E2eError::Timeout(format!(
                "transaction {signature} not confirmed after {timeout:?}"
            )));
        }

        let path = format!("/v1/transaction/{signature}");
        match client.get::<TransactionStatusResponse>(&path).await {
            Ok(status) if status.status == "received" => {
                // Transaction is in mempool but not yet confirmed
                tokio::time::sleep(poll_interval).await;
                poll_interval = (poll_interval * 2).min(max_interval);
            }
            Ok(status) => return Ok(status),
            Err(E2eError::Rpc { status: 404, .. }) => {
                // Not yet indexed, keep polling
                tokio::time::sleep(poll_interval).await;
                poll_interval = (poll_interval * 2).min(max_interval);
            }
            Err(e) => return Err(e),
        }
    }
}

/// Result of a single confirmation attempt within a batch.
#[derive(Debug)]
pub struct BatchConfirmResult {
    pub signature: String,
    pub result: Result<TransactionStatusResponse, E2eError>,
}

/// Poll multiple transaction signatures for confirmation in parallel using `JoinSet`.
///
/// Each signature is polled independently with its own timeout. Returns a result
/// for every signature (either confirmed/failed status or timeout error).
pub async fn wait_for_confirmations_batch(
    client: Arc<NusantaraClient>,
    signatures: Vec<String>,
    timeout: Duration,
) -> Vec<BatchConfirmResult> {
    let mut join_set = JoinSet::new();

    for sig in signatures {
        let client = client.clone();
        let sig_clone = sig.clone();
        join_set.spawn(async move {
            let result = wait_for_confirmation_owned(&client, &sig_clone, timeout).await;
            BatchConfirmResult {
                signature: sig_clone,
                result,
            }
        });
    }

    let mut results = Vec::new();
    while let Some(join_result) = join_set.join_next().await {
        match join_result {
            Ok(confirm_result) => results.push(confirm_result),
            Err(e) => {
                warn!(%e, "confirmation task panicked");
            }
        }
    }

    results
}

/// Same as `wait_for_confirmation` but takes owned/Arc references for use in spawned tasks.
async fn wait_for_confirmation_owned(
    client: &NusantaraClient,
    signature: &str,
    timeout: Duration,
) -> Result<TransactionStatusResponse, E2eError> {
    let start = Instant::now();
    let mut poll_interval = Duration::from_millis(100);
    let max_interval = Duration::from_millis(500);

    loop {
        if start.elapsed() > timeout {
            return Err(E2eError::Timeout(format!(
                "transaction {signature} not confirmed after {timeout:?}"
            )));
        }

        let path = format!("/v1/transaction/{signature}");
        match client.get::<TransactionStatusResponse>(&path).await {
            Ok(status) if status.status == "received" => {
                // Transaction is in mempool but not yet confirmed
                tokio::time::sleep(poll_interval).await;
                poll_interval = (poll_interval * 2).min(max_interval);
            }
            Ok(status) => return Ok(status),
            Err(E2eError::Rpc { status: 404, .. }) => {
                tokio::time::sleep(poll_interval).await;
                poll_interval = (poll_interval * 2).min(max_interval);
            }
            Err(e) => return Err(e),
        }
    }
}
