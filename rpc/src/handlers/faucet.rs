use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Instant;

use axum::Json;
use axum::extract::{ConnectInfo, State};
use nusantara_core::Message;
use nusantara_core::Transaction;
use nusantara_crypto::Hash;
use tokio::sync::{broadcast, mpsc};
use tracing::warn;

use crate::error::RpcError;
use crate::server::{MAX_AIRDROP_LAMPORTS, MAX_CONFIRM_TIMEOUT_MS, PubsubEvent, RpcState};
use crate::types::{
    AirdropAndConfirmRequest, AirdropAndConfirmResponse, AirdropRequest, AirdropResponse,
};

/// Custom extractor that retrieves the client IP from `ConnectInfo` if
/// available, falling back to localhost. Never rejects.
pub struct ClientIp(pub IpAddr);

impl<S: Send + Sync> axum::extract::FromRequestParts<S> for ClientIp {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        _state: &S,
    ) -> Result<Self, Self::Rejection> {
        let ip = parts
            .extensions
            .get::<ConnectInfo<SocketAddr>>()
            .map(|ci| ci.0.ip())
            .unwrap_or(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST));
        Ok(ClientIp(ip))
    }
}

/// Build, sign, and submit an airdrop transaction.
///
/// This function does NOT manage cooldown state — callers must claim cooldowns
/// before calling and release on failure (F4: TOCTOU-safe atomic claim pattern).
///
/// Returns the transaction hash encoded as base64 on success, or an `RpcError`.
/// On error the caller is responsible for releasing any acquired claims.
fn build_and_submit_airdrop(
    state: &RpcState,
    address: &str,
    lamports: u64,
) -> Result<String, RpcError> {
    let faucet_keypair = state
        .faucet_keypair
        .as_ref()
        .ok_or(RpcError::FaucetDisabled)?;

    let to = Hash::from_base64(address)
        .map_err(|e| RpcError::BadRequest(format!("invalid address: {e}")))?;

    if lamports == 0 {
        return Err(RpcError::BadRequest("lamports must be > 0".to_string()));
    }

    if lamports > MAX_AIRDROP_LAMPORTS {
        return Err(RpcError::BadRequest(format!(
            "max airdrop is {MAX_AIRDROP_LAMPORTS} lamports"
        )));
    }

    let from = faucet_keypair.address();

    // Timestamp-based nonce makes each airdrop tx unique so HTTP retries
    // don't produce "duplicate transaction" errors.
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64;
    let nonce_ix = nusantara_compute_budget_program::set_compute_unit_price(nonce);
    let transfer_ix = nusantara_system_program::transfer(&from, &to, lamports);

    let slot_hashes = state.bank.slot_hashes();
    let recent_blockhash = slot_hashes
        .0
        .first()
        .map(|(_, h)| *h)
        .ok_or_else(|| RpcError::Internal("no slot hashes available for faucet".to_string()))?;

    let mut msg = Message::new(&[nonce_ix, transfer_ix], &from)
        .map_err(|e| RpcError::Internal(format!("failed to build message: {e}")))?;
    msg.recent_blockhash = recent_blockhash;

    let mut tx = Transaction::new(msg);
    tx.sign(&[faucet_keypair.as_ref()]);

    let signature = tx.hash().to_base64();

    state
        .mempool
        .insert(tx.clone())
        .map_err(|e| RpcError::Internal(format!("mempool rejected transaction: {e}")))?;

    // Forward via TPU path for leader routing.
    forward_tx(&state.tx_forward_sender, tx, &signature);

    metrics::counter!("nusantara_rpc_airdrops").increment(1);

    Ok(signature)
}

/// Forward `tx` through the TPU sender channel, logging on channel pressure.
/// Silently ignores the case where no sender is configured.
fn forward_tx(
    sender: &Option<mpsc::Sender<Transaction>>,
    tx: Transaction,
    sig: &str,
) {
    if let Some(fwd) = sender {
        match fwd.try_send(tx) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(_)) => {
                metrics::counter!("nusantara_rpc_tx_forward_dropped", "reason" => "full")
                    .increment(1);
                warn!(sig = %sig, "tpu forward channel full, tx in mempool only");
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                metrics::counter!("nusantara_rpc_tx_forward_dropped", "reason" => "closed")
                    .increment(1);
                warn!(sig = %sig, "tpu forward channel closed");
            }
        }
    }
}

#[utoipa::path(
    post,
    path = "/v1/airdrop",
    request_body = AirdropRequest,
    responses(
        (status = 200, description = "Airdrop submitted", body = AirdropResponse),
        (status = 400, description = "Invalid request"),
        (status = 429, description = "Rate limited"),
        (status = 503, description = "Faucet disabled")
    )
)]
pub async fn airdrop(
    State(state): State<Arc<RpcState>>,
    ClientIp(ip): ClientIp,
    Json(req): Json<AirdropRequest>,
) -> Result<Json<AirdropResponse>, RpcError> {
    metrics::counter!("nusantara_rpc_requests", "endpoint" => "airdrop").increment(1);

    let address_hash = Hash::from_base64(&req.address)
        .map_err(|e| RpcError::BadRequest(format!("invalid address: {e}")))?;

    // Atomic claim: address first, then IP.  On IP failure, release address.
    state.claim_faucet_address(&address_hash)?;
    if let Err(e) = state.claim_faucet_ip(ip) {
        state.release_faucet_address(&address_hash);
        return Err(e);
    }

    // Build and submit the transaction.  On failure, release both claims so
    // the user can retry without waiting the full cooldown window.
    match build_and_submit_airdrop(&state, &req.address, req.lamports) {
        Ok(signature) => Ok(Json(AirdropResponse { signature })),
        Err(e) => {
            state.release_faucet_address(&address_hash);
            state.release_faucet_ip(ip);
            Err(e)
        }
    }
}

#[utoipa::path(
    post,
    path = "/v1/airdrop-and-confirm",
    request_body = AirdropAndConfirmRequest,
    responses(
        (status = 200, description = "Airdrop confirmed", body = AirdropAndConfirmResponse),
        (status = 400, description = "Invalid request"),
        (status = 429, description = "Rate limited"),
        (status = 503, description = "Faucet disabled"),
        (status = 504, description = "Confirmation timed out")
    )
)]
pub async fn airdrop_and_confirm(
    State(state): State<Arc<RpcState>>,
    ClientIp(ip): ClientIp,
    Json(req): Json<AirdropAndConfirmRequest>,
) -> Result<Json<AirdropAndConfirmResponse>, RpcError> {
    metrics::counter!("nusantara_rpc_requests", "endpoint" => "airdrop_and_confirm").increment(1);

    let address_hash = Hash::from_base64(&req.address)
        .map_err(|e| RpcError::BadRequest(format!("invalid address: {e}")))?;

    // Atomic claim: address first, then IP.
    state.claim_faucet_address(&address_hash)?;
    if let Err(e) = state.claim_faucet_ip(ip) {
        state.release_faucet_address(&address_hash);
        return Err(e);
    }

    let timeout_ms = req.timeout_ms.min(MAX_CONFIRM_TIMEOUT_MS);
    let timeout = std::time::Duration::from_millis(timeout_ms);
    let start = Instant::now();

    // Subscribe BEFORE submitting to avoid missing a fast confirmation.
    let mut event_rx: broadcast::Receiver<PubsubEvent> = state.pubsub_tx.subscribe();

    // Decode address hash once for storage lookups inside the confirmation loop.
    // (We already decoded it above for cooldown; reuse address_hash.)

    let signature = match build_and_submit_airdrop(&state, &req.address, req.lamports) {
        Ok(sig) => sig,
        Err(e) => {
            state.release_faucet_address(&address_hash);
            state.release_faucet_ip(ip);
            return Err(e);
        }
    };

    let deadline = tokio::time::Instant::now() + timeout;
    let sig_hash =
        Hash::from_base64(&signature).expect("signature we just computed must decode");
    let mut consecutive_lags: u32 = 0;

    loop {
        tokio::select! {
            biased;
            _ = tokio::time::sleep_until(deadline) => {
                // Release claims only if the tx never landed — the tx may still
                // confirm after the deadline, so we poll storage one final time
                // before deciding to release.  If the tx IS in storage, the user
                // should not be able to immediately retry; keep the claim.
                if state.storage.get_transaction_status(&sig_hash).ok().flatten().is_none() {
                    state.release_faucet_address(&address_hash);
                    state.release_faucet_ip(ip);
                }
                return Err(RpcError::Timeout(format!(
                    "airdrop {signature} not confirmed within {timeout_ms}ms"
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
                        metrics::histogram!("nusantara_rpc_airdrop_and_confirm_ms")
                            .record(elapsed as f64);
                        return Ok(Json(AirdropAndConfirmResponse {
                            signature,
                            slot,
                            status: status.clone(),
                            confirmation_time_ms: elapsed,
                        }));
                    }
                    Ok(_) => continue,
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        consecutive_lags += 1;
                        warn!(
                            missed = n,
                            sig = %signature,
                            consecutive_lags,
                            "airdrop-and-confirm subscriber lagged, events dropped"
                        );
                        // After lagging, poll storage once to check if the
                        // confirmation was among the dropped events (F5).
                        if let Ok(Some(meta)) = state.storage.get_transaction_status(&sig_hash) {
                            let elapsed = start.elapsed().as_millis() as u64;
                            metrics::histogram!("nusantara_rpc_airdrop_and_confirm_ms")
                                .record(elapsed as f64);
                            use nusantara_storage::TransactionStatus;
                            let status = match meta.status {
                                TransactionStatus::Success => "success".to_string(),
                                TransactionStatus::Failed(msg) => format!("failed: {msg}"),
                            };
                            return Ok(Json(AirdropAndConfirmResponse {
                                signature,
                                slot: meta.slot,
                                status,
                                confirmation_time_ms: elapsed,
                            }));
                        }
                        // On second consecutive lag, give up to avoid infinite retry.
                        if consecutive_lags >= 2 {
                            // Release claims only if the tx never landed; if it
                            // did land (status is Some) keep the cooldown so the
                            // user cannot immediately re-drain.
                            if state.storage.get_transaction_status(&sig_hash).ok().flatten().is_none() {
                                state.release_faucet_address(&address_hash);
                                state.release_faucet_ip(ip);
                            }
                            return Err(RpcError::Internal(
                                "pubsub subscriber lagged twice consecutively; \
                                 confirmation status unknown"
                                    .to_string(),
                            ));
                        }
                        continue;
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        // The pubsub sender was dropped (validator shutdown).  The
                        // tx may or may not have landed; apply the same storage
                        // check before releasing the cooldown claim.
                        if state.storage.get_transaction_status(&sig_hash).ok().flatten().is_none() {
                            state.release_faucet_address(&address_hash);
                            state.release_faucet_ip(ip);
                        }
                        return Err(RpcError::Internal(
                            "pubsub channel closed while waiting for confirmation".to_string(),
                        ));
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::{FAUCET_COOLDOWN_PER_ADDRESS_SECS, FAUCET_COOLDOWN_PER_IP_SECS};
    use std::net::Ipv4Addr;

    #[test]
    fn faucet_cooldown_constants_are_reasonable() {
        assert_eq!(FAUCET_COOLDOWN_PER_ADDRESS_SECS, 60);
        assert_eq!(FAUCET_COOLDOWN_PER_IP_SECS, 10);
    }

    #[test]
    fn max_airdrop_lamports_is_ten_nusa() {
        // 10 NUSA = 10 * 1_000_000_000 lamports
        assert_eq!(MAX_AIRDROP_LAMPORTS, 10_000_000_000);
    }

    #[test]
    fn client_ip_extractor_fallback() {
        let ip = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let client_ip = ClientIp(ip);
        assert_eq!(client_ip.0, IpAddr::V4(Ipv4Addr::LOCALHOST));
    }
}
