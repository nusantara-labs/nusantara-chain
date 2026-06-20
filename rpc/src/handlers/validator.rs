use std::collections::HashMap;
use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use nusantara_crypto::Hash;

use crate::error::RpcError;
use crate::server::RpcState;
use crate::types::{ValidatorEntry, ValidatorsResponse};

/// Build `ValidatorEntry` list from bank state.
///
/// Uses an O(1) `HashMap` stake lookup (F14) rather than O(N·M) `find`.
pub(crate) fn build_validator_entries(state: &RpcState) -> (u64, Vec<ValidatorEntry>) {
    let vote_states = state.bank.get_all_vote_states();
    let stake_distribution = state.bank.get_stake_distribution();
    let total_active_stake = state.bank.total_active_stake();

    // Build O(1) lookup from stake distribution (F14).
    let stake_map: HashMap<Hash, u64> = stake_distribution.into_iter().collect();

    let mut validators: Vec<ValidatorEntry> = vote_states
        .iter()
        .map(|(vote_account, vs)| {
            let active_stake = stake_map.get(&vs.node_pubkey).copied().unwrap_or(0);
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

    validators.sort_by_key(|v| std::cmp::Reverse(v.active_stake));

    (total_active_stake, validators)
}

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

    let (total_active_stake, validators) = build_validator_entries(&state);

    Ok(Json(ValidatorsResponse {
        total_active_stake,
        validators,
    }))
}
