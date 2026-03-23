use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use nusantara_crypto::Hash;
use nusantara_loader_program::state::LoaderState;

use crate::error::RpcError;
use crate::server::RpcState;
use crate::types::ProgramResponse;

#[utoipa::path(
    get,
    path = "/v1/program/{address}",
    params(
        ("address" = String, Path, description = "Base64 program address")
    ),
    responses(
        (status = 200, description = "Program info", body = ProgramResponse),
        (status = 404, description = "Program not found")
    )
)]
pub async fn get_program(
    State(state): State<Arc<RpcState>>,
    Path(address): Path<String>,
) -> Result<Json<ProgramResponse>, RpcError> {
    metrics::counter!("nusantara_rpc_requests", "endpoint" => "program").increment(1);

    let hash = Hash::from_base64(&address)
        .map_err(|e| RpcError::BadRequest(format!("invalid address: {e}")))?;

    let account = state
        .storage
        .get_account(&hash)?
        .ok_or_else(|| RpcError::NotFound(format!("program {address} not found")))?;

    if !account.executable {
        return Err(RpcError::BadRequest(format!(
            "account {address} is not executable"
        )));
    }

    let loader_state = LoaderState::from_account_data(&account.data)
        .map_err(|e| RpcError::Deserialization(format!("invalid program state: {e}")))?;

    let program_data_address = match &loader_state {
        LoaderState::Program {
            program_data_address,
        } => *program_data_address,
        _ => {
            return Err(RpcError::BadRequest("account is not a Program".to_string()));
        }
    };

    let pd_account = state
        .storage
        .get_account(&program_data_address)?
        .ok_or_else(|| {
            RpcError::NotFound(format!(
                "program data account {} not found",
                program_data_address.to_base64()
            ))
        })?;

    let pd_state = LoaderState::from_account_data(&pd_account.data)
        .map_err(|e| RpcError::Deserialization(format!("invalid program data state: {e}")))?;

    let (deploy_slot, authority, bytecode_size) = match &pd_state {
        LoaderState::ProgramData {
            slot,
            upgrade_authority,
            bytecode_len,
        } => (
            *slot,
            upgrade_authority.as_ref().map(|a| a.to_base64()),
            *bytecode_len as usize,
        ),
        _ => {
            return Err(RpcError::Deserialization(
                "not a ProgramData account".to_string(),
            ));
        }
    };

    Ok(Json(ProgramResponse {
        address,
        executable: true,
        program_data_address: program_data_address.to_base64(),
        authority,
        deploy_slot,
        bytecode_size,
        lamports: account.lamports,
    }))
}
