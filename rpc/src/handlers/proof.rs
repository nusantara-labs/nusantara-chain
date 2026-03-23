use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use nusantara_crypto::Hash;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::error::RpcError;
use crate::server::RpcState;

/// Merkle proof siblings and path data.
#[derive(Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProofData {
    /// Base64 URL-safe no-pad encoded sibling hashes from leaf to root.
    pub siblings: Vec<String>,
    /// Path bits: true if the current node was the right child at each level.
    pub path: Vec<bool>,
    /// Leaf index in the sorted leaf array.
    pub leaf_index: usize,
    /// Total number of leaves when the proof was generated.
    pub total_leaves: usize,
}

/// Response for the account proof endpoint.
#[derive(Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AccountProofResponse {
    pub address: String,
    pub lamports: u64,
    pub owner: String,
    pub executable: bool,
    pub data_len: usize,
    pub proof: ProofData,
    pub state_root: String,
    pub slot: u64,
}

/// Get the account data together with a Merkle proof against the current state root.
///
/// Light clients can use this to verify that a particular account exists
/// in the validator's committed state without downloading the full ledger.
#[utoipa::path(
    get,
    path = "/v1/account/{address}/proof",
    params(
        ("address" = String, Path, description = "Base64 URL-safe no-pad account address")
    ),
    responses(
        (status = 200, description = "Account with Merkle proof", body = AccountProofResponse),
        (status = 404, description = "Account not found or no proof available")
    )
)]
#[tracing::instrument(skip(state), fields(address = %address))]
pub async fn get_account_proof(
    State(state): State<Arc<RpcState>>,
    Path(address): Path<String>,
) -> Result<Json<AccountProofResponse>, RpcError> {
    metrics::counter!("nusantara_rpc_requests", "endpoint" => "account_proof").increment(1);

    let hash = Hash::from_base64(&address)
        .map_err(|e| RpcError::BadRequest(format!("invalid address: {e}")))?;

    // 1. Load account from storage
    let account = state
        .storage
        .get_account(&hash)?
        .ok_or_else(|| RpcError::NotFound(format!("account {address} not found")))?;

    // 2. Generate Merkle proof from the consensus bank's state tree
    let proof = state
        .bank
        .state_proof(&hash)
        .ok_or_else(|| RpcError::NotFound(format!("no state proof available for {address}")))?;

    // 3. Get current state root and slot
    let state_root = state.bank.state_root();
    let slot = state.bank.current_slot();

    Ok(Json(AccountProofResponse {
        address,
        lamports: account.lamports,
        owner: account.owner.to_base64(),
        executable: account.executable,
        data_len: account.data.len(),
        proof: ProofData {
            siblings: proof.siblings.iter().map(|h| h.to_base64()).collect(),
            path: proof.path,
            leaf_index: proof.leaf_index,
            total_leaves: proof.total_leaves,
        },
        state_root: state_root.to_base64(),
        slot,
    }))
}
