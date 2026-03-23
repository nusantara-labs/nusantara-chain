use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use borsh::BorshDeserialize;
use nusantara_crypto::Hash;
use nusantara_stake_program::StakeStateV2;

use crate::error::RpcError;
use crate::server::RpcState;
use crate::types::StakeAccountResponse;

#[utoipa::path(
    get,
    path = "/v1/stake-account/{address}",
    params(
        ("address" = String, Path, description = "Base64 stake account address")
    ),
    responses(
        (status = 200, description = "Stake account details", body = StakeAccountResponse),
        (status = 404, description = "Stake account not found")
    )
)]
pub async fn get_stake_account(
    State(state): State<Arc<RpcState>>,
    Path(address): Path<String>,
) -> Result<Json<StakeAccountResponse>, RpcError> {
    metrics::counter!("nusantara_rpc_requests", "endpoint" => "stake_account").increment(1);

    let hash = Hash::from_base64(&address)
        .map_err(|e| RpcError::BadRequest(format!("invalid address: {e}")))?;

    let account = state
        .storage
        .get_account(&hash)?
        .ok_or_else(|| RpcError::NotFound(format!("account {address} not found")))?;

    let stake_state: StakeStateV2 = BorshDeserialize::deserialize(&mut account.data.as_slice())
        .map_err(|e| RpcError::Deserialization(format!("not a stake account: {e}")))?;

    let response = match &stake_state {
        StakeStateV2::Uninitialized => StakeAccountResponse {
            address,
            lamports: account.lamports,
            state: "uninitialized".to_string(),
            staker: None,
            withdrawer: None,
            voter: None,
            stake: None,
            activation_epoch: None,
            deactivation_epoch: None,
            rent_exempt_reserve: None,
        },
        StakeStateV2::Initialized(meta) => StakeAccountResponse {
            address,
            lamports: account.lamports,
            state: "initialized".to_string(),
            staker: Some(meta.authorized.staker.to_base64()),
            withdrawer: Some(meta.authorized.withdrawer.to_base64()),
            voter: None,
            stake: None,
            activation_epoch: None,
            deactivation_epoch: None,
            rent_exempt_reserve: Some(meta.rent_exempt_reserve),
        },
        StakeStateV2::Stake(meta, stake) => {
            let deactivation = if stake.delegation.deactivation_epoch == u64::MAX {
                None
            } else {
                Some(stake.delegation.deactivation_epoch)
            };
            StakeAccountResponse {
                address,
                lamports: account.lamports,
                state: "delegated".to_string(),
                staker: Some(meta.authorized.staker.to_base64()),
                withdrawer: Some(meta.authorized.withdrawer.to_base64()),
                voter: Some(stake.delegation.voter_pubkey.to_base64()),
                stake: Some(stake.delegation.stake),
                activation_epoch: Some(stake.delegation.activation_epoch),
                deactivation_epoch: deactivation,
                rent_exempt_reserve: Some(meta.rent_exempt_reserve),
            }
        }
        StakeStateV2::RewardsPool => StakeAccountResponse {
            address,
            lamports: account.lamports,
            state: "rewards_pool".to_string(),
            staker: None,
            withdrawer: None,
            voter: None,
            stake: None,
            activation_epoch: None,
            deactivation_epoch: None,
            rent_exempt_reserve: None,
        },
    };

    Ok(Json(response))
}
