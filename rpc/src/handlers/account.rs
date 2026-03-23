use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use nusantara_core::lamports_to_nusa;
use nusantara_crypto::Hash;

use crate::error::RpcError;
use crate::server::RpcState;
use crate::types::AccountResponse;

/// Note: Account state is eventually consistent across nodes.
/// After a transaction confirms, the account state may take 1-2 slots
/// (~400-800ms) to propagate to follower nodes via block replay.
#[utoipa::path(
    get,
    path = "/v1/account/{address}",
    params(
        ("address" = String, Path, description = "Base64 account address")
    ),
    responses(
        (status = 200, description = "Account info", body = AccountResponse),
        (status = 404, description = "Account not found")
    )
)]
pub async fn get_account(
    State(state): State<Arc<RpcState>>,
    Path(address): Path<String>,
) -> Result<Json<AccountResponse>, RpcError> {
    metrics::counter!("nusantara_rpc_requests", "endpoint" => "account").increment(1);

    let hash = Hash::from_base64(&address)
        .map_err(|e| RpcError::BadRequest(format!("invalid address: {e}")))?;

    let account = state
        .storage
        .get_account(&hash)?
        .ok_or_else(|| RpcError::NotFound(format!("account {address} not found")))?;

    Ok(Json(AccountResponse {
        address,
        lamports: account.lamports,
        nusa: lamports_to_nusa(account.lamports),
        owner: account.owner.to_base64(),
        executable: account.executable,
        rent_epoch: account.rent_epoch,
        data_len: account.data.len(),
    }))
}
