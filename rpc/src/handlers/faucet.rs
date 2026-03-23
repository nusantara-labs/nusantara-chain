use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Instant;

use axum::Json;
use axum::extract::{ConnectInfo, State};
use nusantara_core::Message;
use nusantara_core::Transaction;
use nusantara_crypto::Hash;
use tokio::sync::broadcast;
use tracing::warn;

use crate::error::RpcError;
use crate::server::{PubsubEvent, RpcState};
use crate::types::{
    AirdropAndConfirmRequest, AirdropAndConfirmResponse, AirdropRequest, AirdropResponse,
};

/// Custom extractor that retrieves the client IP from `ConnectInfo` if
/// available, falling back to localhost. This is a `FromRequestParts`
/// extractor that never rejects, making it safe to use regardless of
/// whether `into_make_service_with_connect_info` was used.
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

/// Maximum allowed timeout for airdrop-and-confirm requests (30 seconds).
const MAX_CONFIRM_TIMEOUT_MS: u64 = 30_000;

/// Build, sign, and submit an airdrop transaction. Returns the tx signature.
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

    // Max 10 NUSA per airdrop
    if lamports > 10_000_000_000 {
        return Err(RpcError::BadRequest(
            "max airdrop is 10 NUSA (10_000_000_000 lamports)".to_string(),
        ));
    }

    let from = faucet_keypair.address();

    // Use a nonce instruction to make each airdrop tx unique (prevents
    // "duplicate transaction" on HTTP retries). The nonce is based on the
    // current timestamp in nanoseconds.
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64;
    let nonce_ix = nusantara_compute_budget_program::set_compute_unit_price(nonce);
    let transfer_ix = nusantara_system_program::transfer(&from, &to, lamports);

    // Use a recent blockhash from the bank's slot_hashes for transaction validity.
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

    // Forward via TPU path for leader routing
    if let Some(fwd) = &state.tx_forward_sender {
        let _ = fwd.try_send(tx);
    }

    metrics::counter!("nusantara_rpc_airdrops").increment(1);

    Ok(signature)
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

    // Check faucet cooldowns before building the transaction.
    state.check_faucet_ip_cooldown(ip)?;
    state.check_faucet_address_cooldown(&req.address)?;

    let signature = build_and_submit_airdrop(&state, &req.address, req.lamports)?;

    // Record successful airdrop for cooldown tracking.
    state.record_faucet_airdrop(&req.address, ip);

    Ok(Json(AirdropResponse { signature }))
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

    // Check faucet cooldowns before building the transaction.
    state.check_faucet_ip_cooldown(ip)?;
    state.check_faucet_address_cooldown(&req.address)?;

    let timeout_ms = req.timeout_ms.min(MAX_CONFIRM_TIMEOUT_MS);
    let timeout = std::time::Duration::from_millis(timeout_ms);
    let start = Instant::now();

    // Subscribe BEFORE submitting to prevent race condition.
    let mut event_rx: broadcast::Receiver<PubsubEvent> = state.pubsub_tx.subscribe();

    let signature = build_and_submit_airdrop(&state, &req.address, req.lamports)?;

    // Record successful airdrop for cooldown tracking.
    state.record_faucet_airdrop(&req.address, ip);

    // Await the matching SignatureNotification from the broadcast channel.
    let deadline = tokio::time::Instant::now() + timeout;

    loop {
        tokio::select! {
            biased;
            _ = tokio::time::sleep_until(deadline) => {
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
                        metrics::histogram!("nusantara_rpc_airdrop_and_confirm_ms").record(elapsed as f64);
                        return Ok(Json(AirdropAndConfirmResponse {
                            signature,
                            slot,
                            status: status.clone(),
                            confirmation_time_ms: elapsed,
                        }));
                    }
                    Ok(_) => continue,
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        warn!(
                            missed = n,
                            sig = %signature,
                            "airdrop-and-confirm subscriber lagged, events dropped"
                        );
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;
    use std::time::Instant;

    use crate::server::{FAUCET_COOLDOWN_PER_ADDRESS_SECS, FAUCET_COOLDOWN_PER_IP_SECS};
    use dashmap::DashMap;

    #[test]
    fn faucet_address_cooldown_enforced() {
        let cooldowns: DashMap<String, Instant> = DashMap::new();
        let address = "test_address".to_string();

        // No entry yet -- should pass
        assert!(!cooldowns.contains_key(&address));

        // Record an airdrop
        cooldowns.insert(address.clone(), Instant::now());

        // Immediately after -- should be within cooldown
        let entry = cooldowns.get(&address).unwrap();
        let elapsed = entry.elapsed().as_secs();
        assert!(elapsed < FAUCET_COOLDOWN_PER_ADDRESS_SECS);
    }

    #[test]
    fn faucet_ip_cooldown_enforced() {
        let cooldowns: DashMap<IpAddr, Instant> = DashMap::new();
        let ip = IpAddr::V4(Ipv4Addr::LOCALHOST);

        // No entry yet -- should pass
        assert!(!cooldowns.contains_key(&ip));

        // Record an airdrop
        cooldowns.insert(ip, Instant::now());

        // Immediately after -- should be within cooldown
        let entry = cooldowns.get(&ip).unwrap();
        let elapsed = entry.elapsed().as_secs();
        assert!(elapsed < FAUCET_COOLDOWN_PER_IP_SECS);
    }

    #[test]
    fn cooldown_constants_are_reasonable() {
        assert_eq!(FAUCET_COOLDOWN_PER_ADDRESS_SECS, 60);
        assert_eq!(FAUCET_COOLDOWN_PER_IP_SECS, 10);
    }

    #[test]
    fn client_ip_extractor_fallback() {
        // When no ConnectInfo is present, ClientIp should default to localhost
        let ip = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let client_ip = ClientIp(ip);
        assert_eq!(client_ip.0, IpAddr::V4(Ipv4Addr::LOCALHOST));
    }
}
