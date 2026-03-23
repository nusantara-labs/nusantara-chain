use std::sync::Arc;
use std::sync::atomic::Ordering;

use axum::Json;
use axum::extract::State;

use crate::error::RpcError;
use crate::server::RpcState;
use crate::types::HealthResponse;

#[utoipa::path(
    get,
    path = "/v1/health",
    responses(
        (status = 200, description = "Node health", body = HealthResponse)
    )
)]
pub async fn health(State(state): State<Arc<RpcState>>) -> Result<Json<HealthResponse>, RpcError> {
    metrics::counter!("nusantara_rpc_requests", "endpoint" => "health").increment(1);

    let current_slot = state.bank.current_slot();
    let root_slot = state.storage.get_latest_root()?.unwrap_or(0);
    let behind_slots = current_slot.saturating_sub(root_slot);
    let peer_count = state.cluster_info.peer_count();
    let consecutive_skips = state.consecutive_skips.load(Ordering::Relaxed);

    let (epoch, slot_index) = state.epoch_schedule.get_epoch_and_slot_index(current_slot);
    let slots_in_epoch = state.epoch_schedule.slots_per_epoch;
    let epoch_progress_pct = if slots_in_epoch > 0 {
        (slot_index as f64 / slots_in_epoch as f64) * 100.0
    } else {
        0.0
    };

    let total_active_stake = state.bank.total_active_stake();

    let status = if peer_count == 0 {
        "degraded"
    } else if behind_slots > 100 {
        "behind"
    } else {
        "ok"
    };

    Ok(Json(HealthResponse {
        status: status.to_string(),
        slot: current_slot,
        identity: state.identity.to_base64(),
        root_slot,
        behind_slots,
        peer_count,
        epoch,
        epoch_progress_pct,
        consecutive_skips,
        total_active_stake,
    }))
}
