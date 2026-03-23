use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use nusantara_crypto::Hash;
use serde::Deserialize;

use crate::error::RpcError;
use crate::server::RpcState;
use crate::types::{SignatureEntry, SignaturesResponse};

#[derive(Deserialize)]
pub struct SignaturesQuery {
    pub limit: Option<usize>,
}

#[utoipa::path(
    get,
    path = "/v1/signatures/{address}",
    params(
        ("address" = String, Path, description = "Base64 account address"),
        ("limit" = Option<usize>, Query, description = "Max results (default 20)")
    ),
    responses(
        (status = 200, description = "Transaction signatures for address", body = SignaturesResponse)
    )
)]
pub async fn get_signatures(
    State(state): State<Arc<RpcState>>,
    Path(address): Path<String>,
    Query(query): Query<SignaturesQuery>,
) -> Result<Json<SignaturesResponse>, RpcError> {
    metrics::counter!("nusantara_rpc_requests", "endpoint" => "signatures").increment(1);

    let hash = Hash::from_base64(&address)
        .map_err(|e| RpcError::BadRequest(format!("invalid address: {e}")))?;

    let limit = query.limit.unwrap_or(20).min(1000);
    let sigs = state.storage.get_signatures_for_address(&hash, limit)?;

    let signatures: Vec<SignatureEntry> = sigs
        .into_iter()
        .map(|(slot, tx_index, tx_hash)| SignatureEntry {
            signature: tx_hash.to_base64(),
            slot,
            tx_index,
        })
        .collect();

    Ok(Json(SignaturesResponse {
        address,
        signatures,
    }))
}
