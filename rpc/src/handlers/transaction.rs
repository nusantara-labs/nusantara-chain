use std::sync::Arc;
use std::time::Instant;

use axum::Json;
use axum::extract::{Path, State};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use nusantara_core::Transaction;
use nusantara_crypto::Hash;
use nusantara_storage::TransactionStatus;
use tokio::sync::broadcast;
use tracing::warn;

use crate::error::RpcError;
use crate::server::{PubsubEvent, RpcState};
use crate::types::{
    SendAndConfirmRequest, SendAndConfirmResponse, SendTransactionRequest,
    SendTransactionResponse, TransactionStatusResponse,
};

/// Decode a base64 transaction, deserialize it, insert into mempool, and
/// optionally forward via TPU. Returns the decoded `Transaction` and its
/// base64 hash on success.
fn decode_and_submit(
    state: &RpcState,
    encoded: &str,
) -> Result<(Transaction, String), RpcError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|e| RpcError::BadRequest(format!("invalid base64: {e}")))?;

    let tx: Transaction = borsh::from_slice(&bytes)
        .map_err(|e| RpcError::BadRequest(format!("invalid transaction: {e}")))?;

    let signature = tx.hash().to_base64();

    state
        .mempool
        .insert(tx.clone())
        .map_err(|e| RpcError::BadRequest(format!("mempool rejected transaction: {e}")))?;

    // Forward via TPU path for leader routing
    if let Some(fwd) = &state.tx_forward_sender {
        let _ = fwd.try_send(tx.clone());
    }

    metrics::counter!("nusantara_rpc_transactions_submitted").increment(1);

    Ok((tx, signature))
}

#[utoipa::path(
    get,
    path = "/v1/transaction/{hash}",
    params(
        ("hash" = String, Path, description = "Base64 transaction hash")
    ),
    responses(
        (status = 200, description = "Transaction status", body = TransactionStatusResponse),
        (status = 404, description = "Transaction not found")
    )
)]
pub async fn get_transaction(
    State(state): State<Arc<RpcState>>,
    Path(hash): Path<String>,
) -> Result<Json<TransactionStatusResponse>, RpcError> {
    metrics::counter!("nusantara_rpc_requests", "endpoint" => "transaction").increment(1);

    let tx_hash =
        Hash::from_base64(&hash).map_err(|e| RpcError::BadRequest(format!("invalid hash: {e}")))?;

    let meta = match state.storage.get_transaction_status(&tx_hash)? {
        Some(m) => m,
        None => {
            // Check mempool for "received" status
            if state.mempool.contains(&tx_hash) {
                return Ok(Json(TransactionStatusResponse {
                    signature: hash,
                    slot: 0,
                    status: "received".to_string(),
                    fee: 0,
                    pre_balances: vec![],
                    post_balances: vec![],
                    compute_units_consumed: 0,
                }));
            }
            return Err(RpcError::NotFound(format!(
                "transaction {hash} not found"
            )));
        }
    };

    let status_str = match &meta.status {
        TransactionStatus::Success => "success".to_string(),
        TransactionStatus::Failed(msg) => format!("failed: {msg}"),
    };

    Ok(Json(TransactionStatusResponse {
        signature: hash,
        slot: meta.slot,
        status: status_str,
        fee: meta.fee,
        pre_balances: meta.pre_balances,
        post_balances: meta.post_balances,
        compute_units_consumed: meta.compute_units_consumed,
    }))
}

#[utoipa::path(
    post,
    path = "/v1/transaction/send",
    request_body = SendTransactionRequest,
    responses(
        (status = 200, description = "Transaction submitted", body = SendTransactionResponse),
        (status = 400, description = "Invalid transaction")
    )
)]
pub async fn send_transaction(
    State(state): State<Arc<RpcState>>,
    Json(req): Json<SendTransactionRequest>,
) -> Result<Json<SendTransactionResponse>, RpcError> {
    metrics::counter!("nusantara_rpc_requests", "endpoint" => "send_transaction").increment(1);

    let (_tx, signature) = decode_and_submit(&state, &req.transaction)?;

    Ok(Json(SendTransactionResponse {
        signature,
        status: "received".to_string(),
    }))
}

/// Maximum allowed timeout for send-and-confirm requests (30 seconds).
const MAX_CONFIRM_TIMEOUT_MS: u64 = 30_000;

#[utoipa::path(
    post,
    path = "/v1/transaction/send-and-confirm",
    request_body = SendAndConfirmRequest,
    responses(
        (status = 200, description = "Transaction confirmed", body = SendAndConfirmResponse),
        (status = 400, description = "Invalid transaction"),
        (status = 504, description = "Confirmation timed out")
    )
)]
pub async fn send_and_confirm(
    State(state): State<Arc<RpcState>>,
    Json(req): Json<SendAndConfirmRequest>,
) -> Result<Json<SendAndConfirmResponse>, RpcError> {
    metrics::counter!("nusantara_rpc_requests", "endpoint" => "send_and_confirm").increment(1);

    let timeout_ms = req.timeout_ms.min(MAX_CONFIRM_TIMEOUT_MS);
    let timeout = std::time::Duration::from_millis(timeout_ms);
    let start = Instant::now();

    // Subscribe BEFORE inserting into mempool to prevent the race where the tx
    // is confirmed between mempool insert and subscription setup.
    let mut event_rx: broadcast::Receiver<PubsubEvent> = state.pubsub_tx.subscribe();

    let (_tx, signature) = decode_and_submit(&state, &req.transaction)?;

    // Await the matching SignatureNotification from the broadcast channel.
    let deadline = tokio::time::Instant::now() + timeout;

    loop {
        tokio::select! {
            biased;
            _ = tokio::time::sleep_until(deadline) => {
                return Err(RpcError::Timeout(format!(
                    "transaction {signature} not confirmed within {timeout_ms}ms"
                )));
            }
            event = event_rx.recv() => {
                match event {
                    Ok(PubsubEvent::SignatureNotification {
                        signature: ref sig,
                        slot,
                        ref status,
                    }) if *sig == signature => {
                        let elapsed = start.elapsed().as_millis() as u64;
                        metrics::histogram!("nusantara_rpc_send_and_confirm_ms").record(elapsed as f64);
                        return Ok(Json(SendAndConfirmResponse {
                            signature,
                            slot,
                            status: status.clone(),
                            confirmation_time_ms: elapsed,
                        }));
                    }
                    Ok(_) => {
                        // Not our signature, keep waiting
                        continue;
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        warn!(
                            missed = n,
                            sig = %signature,
                            "send-and-confirm subscriber lagged, events dropped"
                        );
                        // Our event may still arrive, keep waiting
                        continue;
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        return Err(RpcError::Internal(
                            "pubsub channel closed while waiting for confirmation".to_string(),
                        ));
                    }
                }
            }
        }
    }
}
