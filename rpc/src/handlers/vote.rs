use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use borsh::BorshDeserialize;
use nusantara_crypto::Hash;
use nusantara_vote_program::VoteState;

use crate::error::RpcError;
use crate::server::RpcState;
use crate::types::{EpochCreditEntry, VoteAccountResponse};

#[utoipa::path(
    get,
    path = "/v1/vote-account/{address}",
    params(
        ("address" = String, Path, description = "Base64 vote account address")
    ),
    responses(
        (status = 200, description = "Vote account details", body = VoteAccountResponse),
        (status = 404, description = "Vote account not found")
    )
)]
pub async fn get_vote_account(
    State(state): State<Arc<RpcState>>,
    Path(address): Path<String>,
) -> Result<Json<VoteAccountResponse>, RpcError> {
    metrics::counter!("nusantara_rpc_requests", "endpoint" => "vote_account").increment(1);

    let hash = Hash::from_base64(&address)
        .map_err(|e| RpcError::BadRequest(format!("invalid address: {e}")))?;

    let account = state
        .storage
        .get_account(&hash)?
        .ok_or_else(|| RpcError::NotFound(format!("account {address} not found")))?;

    let vote_state: VoteState = BorshDeserialize::deserialize(&mut account.data.as_slice())
        .map_err(|e| RpcError::Deserialization(format!("not a vote account: {e}")))?;

    let last_vote_slot = vote_state.votes.last().map(|l| l.slot);

    let epoch_credits: Vec<EpochCreditEntry> = vote_state
        .epoch_credits
        .iter()
        .map(|(epoch, credits, prev_credits)| EpochCreditEntry {
            epoch: *epoch,
            credits: *credits,
            prev_credits: *prev_credits,
        })
        .collect();

    Ok(Json(VoteAccountResponse {
        address,
        lamports: account.lamports,
        node_pubkey: vote_state.node_pubkey.to_base64(),
        authorized_voter: vote_state.authorized_voter.to_base64(),
        authorized_withdrawer: vote_state.authorized_withdrawer.to_base64(),
        commission: vote_state.commission,
        root_slot: vote_state.root_slot,
        last_vote_slot,
        epoch_credits,
    }))
}
