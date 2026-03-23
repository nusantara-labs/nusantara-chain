use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use nusantara_core::lamports_to_nusa;
use nusantara_crypto::Hash;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::error::RpcError;
use crate::server::RpcState;

/// Query parameters shared by both by-owner and by-program endpoints.
#[derive(Deserialize)]
pub struct AccountsByQuery {
    pub limit: Option<usize>,
}

/// A single account entry in the response list.
#[derive(Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AccountsByEntry {
    pub address: String,
    pub lamports: u64,
    pub nusa: f64,
    pub owner: String,
    pub executable: bool,
    pub data_len: usize,
    pub rent_epoch: u64,
}

/// Response returned by both by-owner and by-program queries.
#[derive(Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AccountsByResponse {
    pub accounts: Vec<AccountsByEntry>,
    pub count: usize,
}

#[utoipa::path(
    get,
    path = "/v1/accounts/by-owner/{owner}",
    params(
        ("owner" = String, Path, description = "Base64 owner address (program that owns the accounts)"),
        ("limit" = Option<usize>, Query, description = "Max results (default 100, max 1000)")
    ),
    responses(
        (status = 200, description = "Accounts owned by the given program", body = AccountsByResponse),
        (status = 400, description = "Invalid owner address")
    )
)]
pub async fn get_accounts_by_owner(
    State(state): State<Arc<RpcState>>,
    Path(owner): Path<String>,
    Query(query): Query<AccountsByQuery>,
) -> Result<Json<AccountsByResponse>, RpcError> {
    metrics::counter!("nusantara_rpc_requests", "endpoint" => "accounts_by_owner").increment(1);

    let owner_hash = Hash::from_base64(&owner)
        .map_err(|e| RpcError::BadRequest(format!("invalid owner address: {e}")))?;

    let limit = query.limit.unwrap_or(100).min(1000);

    let accounts = state
        .storage
        .get_accounts_by_owner(&owner_hash, Some(limit))?;

    let entries: Vec<AccountsByEntry> = accounts
        .into_iter()
        .map(|(address, account)| AccountsByEntry {
            address: address.to_base64(),
            lamports: account.lamports,
            nusa: lamports_to_nusa(account.lamports),
            owner: account.owner.to_base64(),
            executable: account.executable,
            data_len: account.data.len(),
            rent_epoch: account.rent_epoch,
        })
        .collect();

    let count = entries.len();
    Ok(Json(AccountsByResponse {
        accounts: entries,
        count,
    }))
}

#[utoipa::path(
    get,
    path = "/v1/accounts/by-program/{program}",
    params(
        ("program" = String, Path, description = "Base64 program address"),
        ("limit" = Option<usize>, Query, description = "Max results (default 100, max 1000)")
    ),
    responses(
        (status = 200, description = "Accounts belonging to the given program", body = AccountsByResponse),
        (status = 400, description = "Invalid program address")
    )
)]
pub async fn get_accounts_by_program(
    State(state): State<Arc<RpcState>>,
    Path(program): Path<String>,
    Query(query): Query<AccountsByQuery>,
) -> Result<Json<AccountsByResponse>, RpcError> {
    metrics::counter!("nusantara_rpc_requests", "endpoint" => "accounts_by_program").increment(1);

    let program_hash = Hash::from_base64(&program)
        .map_err(|e| RpcError::BadRequest(format!("invalid program address: {e}")))?;

    let limit = query.limit.unwrap_or(100).min(1000);

    let accounts = state
        .storage
        .get_accounts_by_program(&program_hash, Some(limit))?;

    let entries: Vec<AccountsByEntry> = accounts
        .into_iter()
        .map(|(address, account)| AccountsByEntry {
            address: address.to_base64(),
            lamports: account.lamports,
            nusa: lamports_to_nusa(account.lamports),
            owner: account.owner.to_base64(),
            executable: account.executable,
            data_len: account.data.len(),
            rent_epoch: account.rent_epoch,
        })
        .collect();

    let count = entries.len();
    Ok(Json(AccountsByResponse {
        accounts: entries,
        count,
    }))
}
