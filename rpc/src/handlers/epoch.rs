use std::sync::Arc;

use axum::Json;
use axum::extract::State;

use crate::error::RpcError;
use crate::server::RpcState;
use crate::types::EpochInfoResponse;

#[utoipa::path(
    get,
    path = "/v1/epoch-info",
    responses(
        (status = 200, description = "Current epoch info", body = EpochInfoResponse)
    )
)]
pub async fn get_epoch_info(
    State(state): State<Arc<RpcState>>,
) -> Result<Json<EpochInfoResponse>, RpcError> {
    metrics::counter!("nusantara_rpc_requests", "endpoint" => "epoch_info").increment(1);

    let clock = state.bank.clock();
    let (epoch, slot_index) = state.epoch_schedule.get_epoch_and_slot_index(clock.slot);

    Ok(Json(EpochInfoResponse {
        epoch,
        slot_index,
        slots_in_epoch: state.epoch_schedule.slots_per_epoch,
        absolute_slot: clock.slot,
        timestamp: clock.unix_timestamp,
        leader_schedule_epoch: clock.leader_schedule_epoch,
    }))
}
