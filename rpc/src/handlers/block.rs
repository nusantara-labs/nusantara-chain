use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use nusantara_storage::TransactionStatus;

use crate::error::RpcError;
use crate::server::RpcState;
use crate::types::{BlockResponse, BlockTransactionEntry, BlockTransactionsResponse};

#[utoipa::path(
    get,
    path = "/v1/block/{slot}",
    params(
        ("slot" = u64, Path, description = "Slot number")
    ),
    responses(
        (status = 200, description = "Block header", body = BlockResponse),
        (status = 404, description = "Block not found")
    )
)]
pub async fn get_block(
    State(state): State<Arc<RpcState>>,
    Path(slot): Path<u64>,
) -> Result<Json<BlockResponse>, RpcError> {
    metrics::counter!("nusantara_rpc_requests", "endpoint" => "block").increment(1);

    let header = state
        .storage
        .get_block_header(slot)?
        .ok_or_else(|| RpcError::NotFound(format!("block at slot {slot} not found")))?;

    Ok(Json(BlockResponse {
        slot: header.slot,
        parent_slot: header.parent_slot,
        parent_hash: header.parent_hash.to_base64(),
        block_hash: header.block_hash.to_base64(),
        timestamp: header.timestamp,
        validator: header.validator.to_base64(),
        transaction_count: header.transaction_count,
        merkle_root: header.merkle_root.to_base64(),
    }))
}

#[utoipa::path(
    get,
    path = "/v1/block/{slot}/transactions",
    params(
        ("slot" = u64, Path, description = "Slot number")
    ),
    responses(
        (status = 200, description = "Block transactions", body = BlockTransactionsResponse),
        (status = 404, description = "Block not found")
    )
)]
pub async fn get_block_transactions(
    State(state): State<Arc<RpcState>>,
    Path(slot): Path<u64>,
) -> Result<Json<BlockTransactionsResponse>, RpcError> {
    metrics::counter!("nusantara_rpc_requests", "endpoint" => "block_transactions").increment(1);

    let block = state
        .storage
        .get_block(slot)?
        .ok_or_else(|| RpcError::NotFound(format!("block at slot {slot} not found")))?;

    let mut entries = Vec::with_capacity(block.transactions.len());

    for (idx, tx) in block.transactions.iter().enumerate() {
        let tx_hash = tx.hash();
        let (status, fee, compute_units) =
            match state.storage.get_transaction_status(&tx_hash)? {
                Some(meta) => {
                    let status_str = match meta.status {
                        TransactionStatus::Success => "success".to_string(),
                        TransactionStatus::Failed(msg) => format!("failed: {msg}"),
                    };
                    (status_str, meta.fee, meta.compute_units_consumed)
                }
                None => ("unknown".to_string(), 0, 0),
            };

        entries.push(BlockTransactionEntry {
            signature: tx_hash.to_base64(),
            tx_index: idx as u32,
            status,
            fee,
            compute_units_consumed: compute_units,
        });
    }

    Ok(Json(BlockTransactionsResponse {
        slot,
        transactions: entries,
    }))
}
