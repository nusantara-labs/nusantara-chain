use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};

use crate::error::RpcError;
use crate::server::RpcState;
use crate::types::{LeaderScheduleResponse, LeaderSlotEntry};

fn build_schedule_response(
    state: &RpcState,
    epoch: u64,
) -> Result<LeaderScheduleResponse, RpcError> {
    // Check cache first
    if let Some(schedule) = state.leader_cache.read().get(&epoch) {
        let first_slot = state.epoch_schedule.get_first_slot_in_epoch(epoch);
        let entries: Vec<LeaderSlotEntry> = schedule
            .slot_leaders
            .iter()
            .enumerate()
            .map(|(i, leader)| LeaderSlotEntry {
                slot: first_slot + i as u64,
                leader: leader.to_base64(),
            })
            .collect();

        return Ok(LeaderScheduleResponse {
            epoch,
            schedule: entries,
        });
    }

    // Compute on-demand
    let stakes = state.bank.get_stake_distribution();
    let schedule = state
        .leader_schedule_generator
        .compute_schedule(epoch, &stakes, &state.genesis_hash)
        .map_err(|e| RpcError::Internal(format!("failed to compute leader schedule: {e}")))?;

    let first_slot = state.epoch_schedule.get_first_slot_in_epoch(epoch);
    let entries: Vec<LeaderSlotEntry> = schedule
        .slot_leaders
        .iter()
        .enumerate()
        .map(|(i, leader)| LeaderSlotEntry {
            slot: first_slot + i as u64,
            leader: leader.to_base64(),
        })
        .collect();

    // Cache for future use
    state.leader_cache.write().insert(epoch, schedule);

    Ok(LeaderScheduleResponse {
        epoch,
        schedule: entries,
    })
}

#[utoipa::path(
    get,
    path = "/v1/leader-schedule",
    responses(
        (status = 200, description = "Leader schedule for current epoch", body = LeaderScheduleResponse)
    )
)]
pub async fn get_leader_schedule(
    State(state): State<Arc<RpcState>>,
) -> Result<Json<LeaderScheduleResponse>, RpcError> {
    metrics::counter!("nusantara_rpc_requests", "endpoint" => "leader_schedule").increment(1);

    let epoch = state.bank.current_epoch();
    let response = build_schedule_response(&state, epoch)?;
    Ok(Json(response))
}

#[utoipa::path(
    get,
    path = "/v1/leader-schedule/{epoch}",
    params(
        ("epoch" = u64, Path, description = "Epoch number")
    ),
    responses(
        (status = 200, description = "Leader schedule for epoch", body = LeaderScheduleResponse)
    )
)]
pub async fn get_leader_schedule_epoch(
    State(state): State<Arc<RpcState>>,
    Path(epoch): Path<u64>,
) -> Result<Json<LeaderScheduleResponse>, RpcError> {
    metrics::counter!("nusantara_rpc_requests", "endpoint" => "leader_schedule_epoch").increment(1);

    let response = build_schedule_response(&state, epoch)?;
    Ok(Json(response))
}
