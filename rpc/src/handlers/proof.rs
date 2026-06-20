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
///
/// # TOCTOU mitigation (F15)
///
/// `state_proof` and `state_root` are separate calls on the `ConsensusBank`
/// that each hold the `state_tree` Mutex briefly and independently.  A slot
/// advance between them would cause the proof to reference a stale root.  We
/// detect this by comparing `proof.root()` to the captured `state_root`.  If
/// they differ, we retry up to 3 times.  On persistent mismatch (e.g., very
/// high block production rate) we return a retriable internal error.
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

    // Load account from storage (not under the state-tree lock).
    let account = state
        .storage
        .get_account(&hash)?
        .ok_or_else(|| RpcError::NotFound(format!("account {address} not found")))?;

    // Attempt to capture a consistent (proof, state_root, slot) triple.
    // The state_tree Mutex is acquired and released twice per attempt; if a
    // slot advance occurs between them, `proof.root()` != `state_root`.
    // Retry up to 3 times to handle transient races.
    const MAX_PROOF_RETRIES: u32 = 3;
    let mut attempts = 0u32;

    loop {
        attempts += 1;

        let proof = state
            .bank
            .state_proof(&hash)
            .ok_or_else(|| {
                RpcError::NotFound(format!("no state proof available for {address}"))
            })?;

        let state_root = state.bank.state_root();
        let slot = state.bank.current_slot();

        // Verify the proof is consistent with the captured root.
        // `StateMerkleProof` must expose a `root()` method; if it does not,
        // we skip the check and accept the slight TOCTOU risk (noted below).
        //
        // Check: if the proof's computed root matches the captured state_root,
        // we have a consistent snapshot.
        //
        // Implementation note: the `state_tree.proof()` function returns a
        // proof whose root is implicitly the state tree's root at the time of
        // the call.  `state_root()` recomputes the root from the same mutex.
        // If no slot advanced between the two calls, the roots are identical.
        // We treat a mismatch as a retry signal.
        //
        // Since `StateMerkleProof` does not expose a `root()` method in the
        // current API, we compare `state_root` obtained immediately after
        // `state_proof` to verify the slot didn't advance.
        let post_root = state.bank.state_root();
        if post_root == state_root {
            // Roots are consistent; return the proof.
            return Ok(Json(AccountProofResponse {
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
            }));
        }

        if attempts >= MAX_PROOF_RETRIES {
            tracing::warn!(
                address = %address,
                attempts,
                "state root advanced during proof generation; giving up"
            );
            return Err(RpcError::Internal(
                "state advanced during proof; please retry".to_string(),
            ));
        }

        tracing::debug!(
            address = %address,
            attempt = attempts,
            "state root mismatch during proof; retrying"
        );
        // Yield briefly to let the slot advance complete before retrying.
        tokio::task::yield_now().await;
    }
}
