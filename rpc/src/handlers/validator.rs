use std::sync::Arc;

use axum::Json;
use axum::extract::State;

use crate::error::RpcError;
use crate::server::RpcState;
use crate::types::{ValidatorEntry, ValidatorsResponse};

#[utoipa::path(
    get,
    path = "/v1/validators",
    responses(
        (status = 200, description = "List of validators", body = ValidatorsResponse)
    )
)]
pub async fn get_validators(
    State(state): State<Arc<RpcState>>,
) -> Result<Json<ValidatorsResponse>, RpcError> {
    metrics::counter!("nusantara_rpc_requests", "endpoint" => "validators").increment(1);

    let vote_states = state.bank.get_all_vote_states();
    let stake_distribution = state.bank.get_stake_distribution();
    let total_active_stake = state.bank.total_active_stake();

    let mut validators: Vec<ValidatorEntry> = vote_states
        .iter()
        .map(|(vote_account, vs)| {
            let active_stake = stake_distribution
                .iter()
                .find(|(id, _)| *id == vs.node_pubkey)
                .map(|(_, s)| *s)
                .unwrap_or(0);

            let last_vote = vs.votes.last().map(|l| l.slot);

            ValidatorEntry {
                identity: vs.node_pubkey.to_base64(),
                vote_account: vote_account.to_base64(),
                commission: vs.commission,
                active_stake,
                last_vote,
                root_slot: vs.root_slot,
            }
        })
        .collect();

    // Sort by stake descending
    validators.sort_by(|a, b| b.active_stake.cmp(&a.active_stake));

    Ok(Json(ValidatorsResponse {
        total_active_stake,
        validators,
    }))
}
