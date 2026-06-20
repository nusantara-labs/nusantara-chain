// JSON-RPC 2.0 dispatch handler for Nusantara blockchain.
//
// Routes incoming JSON-RPC method calls to the appropriate business logic,
// reusing the same storage/bank/mempool layer as the REST handlers.
// Supports both single and batch requests per the JSON-RPC 2.0 specification.
//
// Batch requests are processed concurrently via per-index `JoinHandle`s with
// response ordering and panic recovery guaranteed per JSON-RPC 2.0 §6.

use std::collections::HashMap;
use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use nusantara_core::Transaction;
use nusantara_core::lamports_to_nusa;
use nusantara_crypto::Hash;
use nusantara_storage::TransactionStatus;
use serde_json::Value;
use tokio::sync::mpsc;
use tracing::{debug, warn};

use crate::jsonrpc::{
    INTERNAL_ERROR, INVALID_PARAMS, INVALID_REQUEST, JsonRpcRequest, JsonRpcResponse,
    METHOD_NOT_FOUND, PARSE_ERROR, RESOURCE_NOT_FOUND,
};
use crate::server::{MAX_BATCH_SIZE, RpcState};

// ---------------------------------------------------------------------------
// Parameter helpers
// ---------------------------------------------------------------------------

fn get_string_param(params: &Option<Value>, index: usize) -> Result<String, (i32, String)> {
    let arr = params
        .as_ref()
        .and_then(|p| p.as_array())
        .ok_or_else(|| (INVALID_PARAMS, "params must be an array".to_string()))?;
    arr.get(index)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| {
            (
                INVALID_PARAMS,
                format!("missing string param at index {index}"),
            )
        })
}

fn get_u64_param(params: &Option<Value>, index: usize) -> Result<u64, (i32, String)> {
    let arr = params
        .as_ref()
        .and_then(|p| p.as_array())
        .ok_or_else(|| (INVALID_PARAMS, "params must be an array".to_string()))?;
    arr.get(index).and_then(|v| v.as_u64()).ok_or_else(|| {
        (
            INVALID_PARAMS,
            format!("missing u64 param at index {index}"),
        )
    })
}

fn get_optional_u64_param(params: &Option<Value>, index: usize) -> Option<u64> {
    params.as_ref()?.as_array()?.get(index)?.as_u64()
}

// ---------------------------------------------------------------------------
// Method dispatch
// ---------------------------------------------------------------------------

/// Route a single method name + params to the matching handler.
///
/// Returns `Ok(Value)` on success or `Err((code, message))` on error.
/// This is a synchronous function — no awaits needed by any current handler.
fn dispatch_method(
    state: &RpcState,
    method: &str,
    params: &Option<Value>,
) -> Result<Value, (i32, String)> {
    match method {
        "getHealth" => handle_get_health(),
        "getSlot" => handle_get_slot(state),
        "getLatestBlockhash" => handle_get_latest_blockhash(state),
        "getAccountInfo" => handle_get_account_info(state, params),
        "getBalance" => handle_get_balance(state, params),
        "sendTransaction" => handle_send_transaction(state, params),
        "getTransaction" => handle_get_transaction(state, params),
        "getBlock" => handle_get_block(state, params),
        "getEpochInfo" => handle_get_epoch_info(state),
        "getLeaderSchedule" => handle_get_leader_schedule(state, params),
        "getVoteAccounts" => handle_get_vote_accounts(state),
        "getProgramAccounts" => handle_get_program_accounts(state, params),
        _ => Err((METHOD_NOT_FOUND, format!("method not found: {method}"))),
    }
}

// ---------------------------------------------------------------------------
// Individual method handlers
// ---------------------------------------------------------------------------

fn handle_get_health() -> Result<Value, (i32, String)> {
    Ok(serde_json::json!("ok"))
}

fn handle_get_slot(state: &RpcState) -> Result<Value, (i32, String)> {
    let slot = state.bank.current_slot();
    let latest_stored = state
        .storage
        .get_latest_slot()
        .map_err(|e| (INTERNAL_ERROR, e.to_string()))?;
    let latest_root = state
        .storage
        .get_latest_root()
        .map_err(|e| (INTERNAL_ERROR, e.to_string()))?;

    Ok(serde_json::json!({
        "slot": slot,
        "latest_stored_slot": latest_stored,
        "latest_root": latest_root,
    }))
}

fn handle_get_latest_blockhash(state: &RpcState) -> Result<Value, (i32, String)> {
    let slot_hashes = state.bank.slot_hashes();
    let (slot, hash) = slot_hashes
        .0
        .first()
        .ok_or_else(|| (INTERNAL_ERROR, "no slot hashes available".to_string()))?;

    Ok(serde_json::json!({
        "blockhash": hash.to_base64(),
        "slot": *slot,
    }))
}

fn handle_get_account_info(
    state: &RpcState,
    params: &Option<Value>,
) -> Result<Value, (i32, String)> {
    let addr_str = get_string_param(params, 0)?;
    let hash = Hash::from_base64(&addr_str)
        .map_err(|e| (INVALID_PARAMS, format!("invalid address: {e}")))?;

    let account = state
        .storage
        .get_account(&hash)
        .map_err(|e| (INTERNAL_ERROR, e.to_string()))?
        .ok_or_else(|| (RESOURCE_NOT_FOUND, format!("account not found: {addr_str}")))?;

    Ok(serde_json::json!({
        "address": addr_str,
        "lamports": account.lamports,
        "nusa": lamports_to_nusa(account.lamports),
        "owner": account.owner.to_base64(),
        "executable": account.executable,
        "rent_epoch": account.rent_epoch,
        "data_len": account.data.len(),
    }))
}

fn handle_get_balance(state: &RpcState, params: &Option<Value>) -> Result<Value, (i32, String)> {
    let addr_str = get_string_param(params, 0)?;
    let hash = Hash::from_base64(&addr_str)
        .map_err(|e| (INVALID_PARAMS, format!("invalid address: {e}")))?;

    let account = state
        .storage
        .get_account(&hash)
        .map_err(|e| (INTERNAL_ERROR, e.to_string()))?
        .ok_or_else(|| (RESOURCE_NOT_FOUND, format!("account not found: {addr_str}")))?;

    Ok(serde_json::json!({
        "value": account.lamports,
    }))
}

fn handle_send_transaction(
    state: &RpcState,
    params: &Option<Value>,
) -> Result<Value, (i32, String)> {
    let b64 = get_string_param(params, 0)?;

    let bytes = URL_SAFE_NO_PAD
        .decode(&b64)
        .map_err(|e| (INVALID_PARAMS, format!("invalid base64: {e}")))?;

    let tx: Transaction = borsh::from_slice(&bytes)
        .map_err(|e| (INVALID_PARAMS, format!("invalid transaction: {e}")))?;

    let signature = tx.hash().to_base64();

    state
        .mempool
        .insert(tx.clone())
        .map_err(|e| (INTERNAL_ERROR, format!("mempool rejected transaction: {e}")))?;

    // Forward via TPU path (fire-and-forget with logging on pressure).
    if let Some(fwd) = &state.tx_forward_sender {
        match fwd.try_send(tx) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(_)) => {
                metrics::counter!("nusantara_rpc_tx_forward_dropped", "reason" => "full")
                    .increment(1);
                warn!(sig = %signature, "tpu forward channel full, tx in mempool only");
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                metrics::counter!("nusantara_rpc_tx_forward_dropped", "reason" => "closed")
                    .increment(1);
                warn!(sig = %signature, "tpu forward channel closed");
            }
        }
    }

    metrics::counter!("nusantara_rpc_jsonrpc_transactions_submitted").increment(1);

    Ok(serde_json::json!(signature))
}

fn handle_get_transaction(
    state: &RpcState,
    params: &Option<Value>,
) -> Result<Value, (i32, String)> {
    let hash_str = get_string_param(params, 0)?;
    let tx_hash = Hash::from_base64(&hash_str)
        .map_err(|e| (INVALID_PARAMS, format!("invalid hash: {e}")))?;

    let meta = match state
        .storage
        .get_transaction_status(&tx_hash)
        .map_err(|e| (INTERNAL_ERROR, e.to_string()))?
    {
        Some(m) => m,
        None => {
            if state.mempool.contains(&tx_hash) {
                return Ok(serde_json::json!({
                    "signature": hash_str,
                    "slot": 0,
                    "status": "received",
                    "fee": 0,
                    "pre_balances": [],
                    "post_balances": [],
                    "compute_units_consumed": 0,
                }));
            }
            return Err((
                RESOURCE_NOT_FOUND,
                format!("transaction not found: {hash_str}"),
            ));
        }
    };

    let status_str = match &meta.status {
        TransactionStatus::Success => "success".to_string(),
        TransactionStatus::Failed(msg) => format!("failed: {msg}"),
    };

    Ok(serde_json::json!({
        "signature": hash_str,
        "slot": meta.slot,
        "status": status_str,
        "fee": meta.fee,
        "pre_balances": meta.pre_balances,
        "post_balances": meta.post_balances,
        "compute_units_consumed": meta.compute_units_consumed,
    }))
}

fn handle_get_block(state: &RpcState, params: &Option<Value>) -> Result<Value, (i32, String)> {
    let slot = get_u64_param(params, 0)?;

    let header = state
        .storage
        .get_block_header(slot)
        .map_err(|e| (INTERNAL_ERROR, e.to_string()))?
        .ok_or_else(|| (RESOURCE_NOT_FOUND, format!("block at slot {slot} not found")))?;

    Ok(serde_json::json!({
        "slot": header.slot,
        "parent_slot": header.parent_slot,
        "parent_hash": header.parent_hash.to_base64(),
        "block_hash": header.block_hash.to_base64(),
        "timestamp": header.timestamp,
        "validator": header.validator.to_base64(),
        "transaction_count": header.transaction_count,
        "merkle_root": header.merkle_root.to_base64(),
    }))
}

fn handle_get_epoch_info(state: &RpcState) -> Result<Value, (i32, String)> {
    let clock = state.bank.clock();
    let (epoch, slot_index) = state.epoch_schedule.get_epoch_and_slot_index(clock.slot);

    Ok(serde_json::json!({
        "epoch": epoch,
        "slot_index": slot_index,
        "slots_in_epoch": state.epoch_schedule.slots_per_epoch,
        "absolute_slot": clock.slot,
        "timestamp": clock.unix_timestamp,
        "leader_schedule_epoch": clock.leader_schedule_epoch,
    }))
}

/// Validate that `epoch` is within the window we can serve accurately.
///
/// Only `[current_epoch.saturating_sub(2) ..= current_epoch + 2]` is
/// supported because we do not snapshot stake distributions per epoch.
fn validate_epoch_range(state: &RpcState, epoch: u64) -> Result<(), (i32, String)> {
    let current = state.bank.current_epoch();
    let lo = current.saturating_sub(2);
    let hi = current + 2;
    if epoch < lo || epoch > hi {
        return Err((
            INVALID_PARAMS,
            format!(
                "epoch {epoch} out of range: only [{lo}, {hi}] is supported \
                 (historical stake distributions are not stored per epoch)"
            ),
        ));
    }
    Ok(())
}

fn handle_get_leader_schedule(
    state: &RpcState,
    params: &Option<Value>,
) -> Result<Value, (i32, String)> {
    let epoch = get_optional_u64_param(params, 0).unwrap_or_else(|| state.bank.current_epoch());

    validate_epoch_range(state, epoch)?;

    // Check LRU cache first (Mutex, never held across await).
    {
        let mut cache = state.leader_cache.lock();
        if let Some(schedule) = cache.get(&epoch) {
            let first_slot = state.epoch_schedule.get_first_slot_in_epoch(epoch);
            let entries: Vec<Value> = schedule
                .slot_leaders
                .iter()
                .enumerate()
                .map(|(i, leader)| {
                    serde_json::json!({
                        "slot": first_slot + i as u64,
                        "leader": leader.to_base64(),
                    })
                })
                .collect();

            return Ok(serde_json::json!({
                "epoch": epoch,
                "schedule": entries,
            }));
        }
    } // lock released here

    let stakes = state.bank.get_stake_distribution();
    let schedule = state
        .leader_schedule_generator
        .compute_schedule(epoch, &stakes, &state.genesis_hash)
        .map_err(|e| {
            (
                INTERNAL_ERROR,
                format!("failed to compute leader schedule: {e}"),
            )
        })?;

    let first_slot = state.epoch_schedule.get_first_slot_in_epoch(epoch);
    let entries: Vec<Value> = schedule
        .slot_leaders
        .iter()
        .enumerate()
        .map(|(i, leader)| {
            serde_json::json!({
                "slot": first_slot + i as u64,
                "leader": leader.to_base64(),
            })
        })
        .collect();

    state.leader_cache.lock().put(epoch, schedule);

    Ok(serde_json::json!({
        "epoch": epoch,
        "schedule": entries,
    }))
}

fn handle_get_vote_accounts(state: &RpcState) -> Result<Value, (i32, String)> {
    let vote_states = state.bank.get_all_vote_states();
    let stake_distribution = state.bank.get_stake_distribution();
    let total_active_stake = state.bank.total_active_stake();

    // Build a O(1) lookup map from the stake distribution (F14).
    let stake_map: HashMap<Hash, u64> = stake_distribution.into_iter().collect();

    let mut validators: Vec<Value> = vote_states
        .iter()
        .map(|(vote_account, vs)| {
            let active_stake = stake_map.get(&vs.node_pubkey).copied().unwrap_or(0);
            let last_vote = vs.votes.last().map(|l| l.slot);

            serde_json::json!({
                "identity": vs.node_pubkey.to_base64(),
                "vote_account": vote_account.to_base64(),
                "commission": vs.commission,
                "active_stake": active_stake,
                "last_vote": last_vote,
                "root_slot": vs.root_slot,
            })
        })
        .collect();

    validators.sort_by(|a, b| {
        let sa = a["active_stake"].as_u64().unwrap_or(0);
        let sb = b["active_stake"].as_u64().unwrap_or(0);
        sb.cmp(&sa)
    });

    Ok(serde_json::json!({
        "total_active_stake": total_active_stake,
        "validators": validators,
    }))
}

fn handle_get_program_accounts(
    state: &RpcState,
    params: &Option<Value>,
) -> Result<Value, (i32, String)> {
    let program_str = get_string_param(params, 0)?;
    let program_hash = Hash::from_base64(&program_str)
        .map_err(|e| (INVALID_PARAMS, format!("invalid program address: {e}")))?;

    let accounts = state
        .storage
        .get_accounts_by_owner(&program_hash, Some(1000))
        .map_err(|e| (INTERNAL_ERROR, e.to_string()))?;

    let entries: Vec<Value> = accounts
        .into_iter()
        .map(|(address, account)| {
            serde_json::json!({
                "address": address.to_base64(),
                "lamports": account.lamports,
                "nusa": lamports_to_nusa(account.lamports),
                "owner": account.owner.to_base64(),
                "executable": account.executable,
                "data_len": account.data.len(),
                "rent_epoch": account.rent_epoch,
            })
        })
        .collect();

    let count = entries.len();
    Ok(serde_json::json!({
        "accounts": entries,
        "count": count,
    }))
}

// ---------------------------------------------------------------------------
// Top-level handler
// ---------------------------------------------------------------------------

/// Process a single JSON-RPC request value.
///
/// Returns `Some(JsonRpcResponse)` for normal requests and `None` for
/// notifications (requests without an `id` field), which per JSON-RPC 2.0
/// must not receive a response (F12).
fn process_single_request(state: &RpcState, value: Value) -> Option<JsonRpcResponse> {
    let req: JsonRpcRequest = match serde_json::from_value(value) {
        Ok(r) => r,
        Err(_) => {
            return Some(JsonRpcResponse::error(
                Value::Null,
                INVALID_REQUEST,
                "invalid request object".to_string(),
            ));
        }
    };

    // Notifications (no id) must not receive a response.
    let is_notification = req.id.is_none();
    let id = req.id.unwrap_or(Value::Null);

    if req.jsonrpc != "2.0" {
        if is_notification {
            return None;
        }
        return Some(JsonRpcResponse::error(
            id,
            INVALID_REQUEST,
            "jsonrpc must be \"2.0\"".to_string(),
        ));
    }

    debug!(method = %req.method, "JSON-RPC dispatch");

    let result = dispatch_method(state, &req.method, &req.params);

    if is_notification {
        // Fire-and-forget: execute the method but do not reply.
        if let Err((code, message)) = result
            && code == INTERNAL_ERROR
        {
            warn!(
                method = %req.method,
                error = %message,
                "JSON-RPC notification internal error"
            );
        }
        return None;
    }

    match result {
        Ok(value) => Some(JsonRpcResponse::success(id, value)),
        Err((code, message)) => {
            if code == INTERNAL_ERROR {
                warn!(method = %req.method, error = %message, "JSON-RPC internal error");
            }
            Some(JsonRpcResponse::error(id, code, message))
        }
    }
}

/// Axum handler for `POST /rpc`.
///
/// Accepts a raw `Json<Value>` so that we can distinguish between:
/// - a single JSON object (single request or notification)
/// - a JSON array (batch request)
/// - anything else (parse error)
///
/// Batch requests are processed concurrently; response order matches request
/// order (per JSON-RPC 2.0 spec).
///
/// If all items in a batch are notifications, HTTP 204 No Content is returned.
/// Single notifications also return HTTP 204.
#[tracing::instrument(skip_all, name = "jsonrpc_handler")]
pub async fn handle_jsonrpc(
    State(state): State<Arc<RpcState>>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    metrics::counter!("nusantara_rpc_jsonrpc_requests").increment(1);

    if let Some(arr) = body.as_array().cloned() {
        // Batch request
        if arr.is_empty() {
            return (
                StatusCode::OK,
                Json(
                    serde_json::to_value(JsonRpcResponse::error(
                        Value::Null,
                        INVALID_REQUEST,
                        "empty batch".to_string(),
                    ))
                    .expect("JsonRpcResponse serialization cannot fail"),
                ),
            )
                .into_response();
        }

        if arr.len() > MAX_BATCH_SIZE {
            warn!(
                batch_size = arr.len(),
                max = MAX_BATCH_SIZE,
                "JSON-RPC batch size limit exceeded"
            );
            metrics::counter!("nusantara_rpc_jsonrpc_batch_rejected").increment(1);
            return (
                StatusCode::OK,
                Json(
                    serde_json::to_value(JsonRpcResponse::error(
                        Value::Null,
                        INVALID_REQUEST,
                        format!(
                            "batch too large: {len} requests exceeds maximum of {MAX_BATCH_SIZE}",
                            len = arr.len()
                        ),
                    ))
                    .expect("JsonRpcResponse serialization cannot fail"),
                ),
            )
                .into_response();
        }

        // Process batch concurrently, preserving order and ensuring every
        // non-notification request gets a response even when a task panics
        // (JSON-RPC 2.0 §6).
        let n = arr.len();
        // Use (idx, JoinHandle) so a panic recovery can fill the correct slot.
        let mut handles: Vec<(usize, tokio::task::JoinHandle<Option<JsonRpcResponse>>)> =
            Vec::with_capacity(n);
        for (idx, item) in arr.into_iter().enumerate() {
            let state = Arc::clone(&state);
            handles.push((
                idx,
                tokio::spawn(async move { process_single_request(&state, item) }),
            ));
        }

        // Slots are pre-allocated so each index can be filled independently.
        // `JsonRpcResponse` is not Clone, so use `repeat_with` instead of `vec![None; n]`.
        let mut slots: Vec<Option<JsonRpcResponse>> =
            std::iter::repeat_with(|| None).take(n).collect();
        for (idx, handle) in handles {
            match handle.await {
                Ok(opt) => slots[idx] = opt,
                Err(e) if e.is_panic() => {
                    tracing::error!(idx, error = %e, "JSON-RPC batch task panicked");
                    slots[idx] = Some(JsonRpcResponse::error(
                        Value::Null,
                        INTERNAL_ERROR,
                        "internal error".to_string(),
                    ));
                }
                Err(e) => {
                    tracing::warn!(idx, error = %e, "JSON-RPC batch task join error");
                    slots[idx] = Some(JsonRpcResponse::error(
                        Value::Null,
                        INTERNAL_ERROR,
                        "internal error".to_string(),
                    ));
                }
            }
        }

        let responses: Vec<JsonRpcResponse> = slots.into_iter().flatten().collect();

        metrics::counter!("nusantara_rpc_jsonrpc_batch_requests").increment(1);
        metrics::histogram!("nusantara_rpc_jsonrpc_batch_size").record(responses.len() as f64);

        if responses.is_empty() {
            // All items were notifications — return no body per spec.
            return StatusCode::NO_CONTENT.into_response();
        }

        (
            StatusCode::OK,
            Json(
                serde_json::to_value(responses)
                    .expect("Vec<JsonRpcResponse> serialization cannot fail"),
            ),
        )
            .into_response()
    } else if body.is_object() {
        // Single request or notification
        match process_single_request(&state, body) {
            Some(resp) => (
                StatusCode::OK,
                Json(
                    serde_json::to_value(resp)
                        .expect("JsonRpcResponse serialization cannot fail"),
                ),
            )
                .into_response(),
            None => StatusCode::NO_CONTENT.into_response(),
        }
    } else {
        // Neither object nor array — parse error
        (
            StatusCode::OK,
            Json(
                serde_json::to_value(JsonRpcResponse::error(
                    Value::Null,
                    PARSE_ERROR,
                    "invalid JSON-RPC request".to_string(),
                ))
                .expect("JsonRpcResponse serialization cannot fail"),
            ),
        )
            .into_response()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_string_param_success() {
        let params = Some(serde_json::json!(["abc", "def"]));
        assert_eq!(get_string_param(&params, 0).unwrap(), "abc");
        assert_eq!(get_string_param(&params, 1).unwrap(), "def");
    }

    #[test]
    fn get_string_param_missing() {
        let params = Some(serde_json::json!(["abc"]));
        let err = get_string_param(&params, 1).unwrap_err();
        assert_eq!(err.0, INVALID_PARAMS);
    }

    #[test]
    fn get_string_param_not_array() {
        let params = Some(serde_json::json!({"key": "val"}));
        let err = get_string_param(&params, 0).unwrap_err();
        assert_eq!(err.0, INVALID_PARAMS);
    }

    #[test]
    fn get_string_param_none() {
        let err = get_string_param(&None, 0).unwrap_err();
        assert_eq!(err.0, INVALID_PARAMS);
    }

    #[test]
    fn get_u64_param_success() {
        let params = Some(serde_json::json!([42]));
        assert_eq!(get_u64_param(&params, 0).unwrap(), 42);
    }

    #[test]
    fn get_u64_param_not_number() {
        let params = Some(serde_json::json!(["not_a_number"]));
        let err = get_u64_param(&params, 0).unwrap_err();
        assert_eq!(err.0, INVALID_PARAMS);
    }

    #[test]
    fn get_optional_u64_param_present() {
        let params = Some(serde_json::json!([10]));
        assert_eq!(get_optional_u64_param(&params, 0), Some(10));
    }

    #[test]
    fn get_optional_u64_param_absent() {
        let params = Some(serde_json::json!([]));
        assert_eq!(get_optional_u64_param(&params, 0), None);
    }

    #[test]
    fn get_optional_u64_param_none_params() {
        assert_eq!(get_optional_u64_param(&None, 0), None);
    }

    #[test]
    fn process_request_invalid_json_structure() {
        let value = serde_json::json!({"not": "valid"});
        let result: Result<JsonRpcRequest, _> = serde_json::from_value(value);
        assert!(result.is_err());
    }

    #[test]
    fn process_request_wrong_jsonrpc_version() {
        let json = serde_json::json!({
            "jsonrpc": "1.0",
            "method": "getSlot",
            "id": 1
        });
        let req: JsonRpcRequest = serde_json::from_value(json).unwrap();
        assert_ne!(req.jsonrpc, "2.0");
    }

    #[test]
    fn batch_detection_array() {
        let body = serde_json::json!([
            {"jsonrpc": "2.0", "method": "getSlot", "id": 1},
            {"jsonrpc": "2.0", "method": "getHealth", "id": 2}
        ]);
        assert!(body.is_array());
        assert_eq!(body.as_array().unwrap().len(), 2);
    }

    #[test]
    fn batch_detection_single() {
        let body = serde_json::json!({"jsonrpc": "2.0", "method": "getSlot", "id": 1});
        assert!(body.is_object());
        assert!(!body.is_array());
    }

    #[test]
    fn batch_detection_invalid() {
        let body = serde_json::json!("just a string");
        assert!(!body.is_object());
        assert!(!body.is_array());
    }

    #[test]
    fn dispatch_unknown_method_error_code() {
        assert_eq!(METHOD_NOT_FOUND, -32601);
    }

    #[test]
    fn max_batch_size_constant() {
        assert_eq!(MAX_BATCH_SIZE, 100);
    }

    #[test]
    fn resource_not_found_error_code() {
        assert_eq!(RESOURCE_NOT_FOUND, -32004);
    }

    #[test]
    fn batch_within_limit_is_accepted() {
        let items: Vec<Value> = (0..MAX_BATCH_SIZE)
            .map(|i| {
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "method": "getHealth",
                    "id": i
                })
            })
            .collect();
        assert!(items.len() <= MAX_BATCH_SIZE);
    }

    #[test]
    fn batch_exceeding_limit_is_detected() {
        let items: Vec<Value> = (0..=MAX_BATCH_SIZE)
            .map(|i| {
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "method": "getHealth",
                    "id": i
                })
            })
            .collect();
        assert!(items.len() > MAX_BATCH_SIZE);
    }

    #[test]
    fn notification_has_no_id() {
        let json = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "notify",
            "params": [1]
        });
        let req: JsonRpcRequest = serde_json::from_value(json).unwrap();
        assert!(req.id.is_none());
    }
}
