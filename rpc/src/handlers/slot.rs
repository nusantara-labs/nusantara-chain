use std::sync::Arc;

use axum::Json;
use axum::extract::State;

use crate::error::RpcError;
use crate::server::RpcState;
use crate::types::{BlockhashResponse, SlotResponse};

#[utoipa::path(
    get,
    path = "/v1/slot",
    responses(
        (status = 200, description = "Current slot info", body = SlotResponse)
    )
)]
pub async fn get_slot(State(state): State<Arc<RpcState>>) -> Result<Json<SlotResponse>, RpcError> {
    metrics::counter!("nusantara_rpc_requests", "endpoint" => "slot").increment(1);

    let slot = state.bank.current_slot();
    let latest_stored_slot = state.storage.get_latest_slot()?;
    let latest_root = state.storage.get_latest_root()?;

    Ok(Json(SlotResponse {
        slot,
        latest_stored_slot,
        latest_root,
    }))
}

#[utoipa::path(
    get,
    path = "/v1/blockhash",
    responses(
        (status = 200, description = "Recent blockhash for transaction signing", body = BlockhashResponse)
    )
)]
pub async fn get_blockhash(
    State(state): State<Arc<RpcState>>,
) -> Result<Json<BlockhashResponse>, RpcError> {
    metrics::counter!("nusantara_rpc_requests", "endpoint" => "blockhash").increment(1);

    let slot_hashes = state.bank.slot_hashes();

    // Use a blockhash a few slots behind the tip: close enough to the tip
    // for long validity (won't be pruned as root advances), but deep enough
    // to have propagated to all validators via Turbine (~1-2 slots).
    let depth = slot_hashes.0.len().clamp(1, 5) - 1;
    let (slot, hash) = slot_hashes
        .0
        .get(depth)
        .or(slot_hashes.0.first())
        .ok_or_else(|| RpcError::Internal("no slot hashes available".to_string()))?;

    Ok(Json(BlockhashResponse {
        blockhash: hash.to_base64(),
        slot: *slot,
    }))
}
