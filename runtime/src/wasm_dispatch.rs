use nusantara_core::Account;
use nusantara_core::program::LOADER_PROGRAM_ID;
use nusantara_crypto::Hash;
use nusantara_loader_program::state::LoaderState;
use nusantara_vm::{ProgramCache, VmHostState, WasmExecutor};
use tracing::instrument;

use crate::error::RuntimeError;
use crate::sysvar_cache::SysvarCache;
use crate::transaction_context::TransactionContext;

/// Dispatch a WASM program invocation.
///
/// Steps:
/// 1. Look up program_id account in TransactionContext
/// 2. Verify executable == true and owner == LOADER_PROGRAM_ID
/// 3. Deserialize LoaderState::Program to get program_data_address
/// 4. Look up ProgramData account, extract bytecode
/// 5. Call WasmExecutor::execute()
/// 6. Map VmError → RuntimeError
#[instrument(skip_all, fields(program = %program_id))]
pub fn dispatch_wasm_program(
    program_id: &nusantara_crypto::Hash,
    accounts: &[u8],
    data: &[u8],
    ctx: &mut TransactionContext,
    _sysvars: &SysvarCache,
    program_cache: &ProgramCache,
) -> Result<(), RuntimeError> {
    // 1. Find the program account in the transaction
    let (_program_data_address, bytecode) = {
        let mut found_program = None;
        for i in 0..ctx.account_count() {
            let acc = ctx.get_account(i)?;
            if acc.address == program_id {
                found_program = Some(i);
                break;
            }
        }

        let program_idx = found_program.ok_or_else(|| {
            RuntimeError::ProgramNotExecutable(format!("program account not found: {}", program_id))
        })?;

        let program_acc = ctx.get_account(program_idx)?;

        // 2. Verify executable and owner
        if !program_acc.account.executable {
            return Err(RuntimeError::ProgramNotExecutable(format!(
                "program {} is not executable",
                program_id
            )));
        }
        if program_acc.account.owner != *LOADER_PROGRAM_ID {
            return Err(RuntimeError::ProgramNotExecutable(format!(
                "program {} not owned by loader",
                program_id
            )));
        }

        // 3. Deserialize LoaderState::Program
        let state = LoaderState::from_account_data(&program_acc.account.data)
            .map_err(RuntimeError::InvalidAccountData)?;

        let pd_address = match state {
            LoaderState::Program {
                program_data_address,
            } => program_data_address,
            _ => {
                return Err(RuntimeError::ProgramNotExecutable(
                    "not a Program account".to_string(),
                ));
            }
        };

        // 4. Find ProgramData account and extract bytecode
        let mut found_pd = None;
        for i in 0..ctx.account_count() {
            let acc = ctx.get_account(i)?;
            if *acc.address == pd_address {
                found_pd = Some(i);
                break;
            }
        }

        let pd_idx = found_pd.ok_or_else(|| {
            RuntimeError::InvalidAccountData(
                "program data account not found in transaction".to_string(),
            )
        })?;

        let pd_acc = ctx.get_account(pd_idx)?;
        let bytecode = LoaderState::extract_bytecode(&pd_acc.account.data)
            .map_err(RuntimeError::InvalidAccountData)?
            .to_vec();

        (pd_address, bytecode)
    };

    // 5. Build account indices mapping for WASM (which accounts the program can access)
    let account_indices: Vec<usize> = accounts.iter().map(|&i| i as usize).collect();
    let account_privileges: Vec<(bool, bool)> = account_indices
        .iter()
        .map(|&i| {
            let acc = ctx.get_account(i)?;
            Ok((acc.is_signer, acc.is_writable))
        })
        .collect::<Result<Vec<_>, RuntimeError>>()?;

    // 6. Prepare host state and execute
    let compute_remaining = ctx.compute_remaining();
    let slot = ctx.slot;

    let (result, compute_after) = {
        let raw_accounts = ctx.accounts_mut();

        let mut host_state = VmHostState::new(
            raw_accounts,
            &account_privileges,
            account_indices,
            *program_id,
            program_cache,
            slot,
            compute_remaining,
        )
        .with_dispatch_fn(cpi_dispatch);

        let r = WasmExecutor::execute(&bytecode, program_id, data, &mut host_state, program_cache);

        (r, host_state.compute_remaining)
    };

    // 7. Sync compute consumption back
    ctx.set_compute_remaining(compute_after);

    // 8. Map VmError → RuntimeError
    match result {
        Ok(_) => Ok(()),
        Err(e) => Err(RuntimeError::WasmError(e.to_string())),
    }
}

/// CPI dispatch function matching the [`nusantara_vm::host_state::DispatchFn`] signature.
///
/// This function is registered with [`VmHostState::with_dispatch_fn`] so that
/// when a WASM program issues a cross-program invocation via `nusa_invoke`, the
/// VM can call back into the runtime to execute the target program.
///
/// The function operates on the raw accounts slice and compute counter that live
/// inside the outer [`VmHostState`], avoiding any dependency on
/// [`TransactionContext`] (which cannot be passed through the `fn` pointer
/// boundary).
#[allow(clippy::too_many_arguments)]
fn cpi_dispatch(
    program_id: &Hash,
    account_indices: &[usize],
    instruction_data: &[u8],
    accounts: &mut [(Hash, Account)],
    account_privileges: &[(bool, bool)],
    compute_remaining: &mut u64,
    slot: u64,
    program_cache: &ProgramCache,
    cpi_depth: u32,
    call_stack: &mut Vec<Hash>,
) -> Result<(), String> {
    // 1. Find the target program in the accounts list.
    let program_acc_idx = accounts
        .iter()
        .position(|(addr, _)| addr == program_id)
        .ok_or_else(|| format!("CPI target program account not found: {program_id}"))?;

    let (_, ref program_account) = accounts[program_acc_idx];

    // 2. Verify the program is executable and owned by the loader.
    if !program_account.executable {
        return Err(format!("CPI target {program_id} is not executable"));
    }
    if program_account.owner != *LOADER_PROGRAM_ID {
        return Err(format!("CPI target {program_id} not owned by loader"));
    }

    // 3. Deserialize LoaderState::Program to find the ProgramData address.
    let state = LoaderState::from_account_data(&program_account.data)
        .map_err(|e| format!("failed to deserialize program state: {e}"))?;
    let pd_address = match state {
        LoaderState::Program {
            program_data_address,
        } => program_data_address,
        _ => return Err("CPI target is not a Program account".to_string()),
    };

    // 4. Find the ProgramData account and extract bytecode.
    let pd_idx = accounts
        .iter()
        .position(|(addr, _)| *addr == pd_address)
        .ok_or_else(|| format!("CPI program data account not found for {program_id}"))?;
    let bytecode = LoaderState::extract_bytecode(&accounts[pd_idx].1.data)
        .map_err(|e| format!("failed to extract bytecode: {e}"))?
        .to_vec();

    // 5. Build privilege metadata for the passed account indices.
    let cpi_privileges: Vec<(bool, bool)> = account_indices
        .iter()
        .map(|&i| {
            if i < account_privileges.len() {
                account_privileges[i]
            } else {
                (false, false)
            }
        })
        .collect();

    // 6. Create a nested VmHostState at depth+1 with the inherited call stack.
    let mut host_state = VmHostState::new(
        accounts,
        &cpi_privileges,
        account_indices.to_vec(),
        *program_id,
        program_cache,
        slot,
        *compute_remaining,
    )
    .with_dispatch_fn(cpi_dispatch)
    .with_cpi_depth(cpi_depth)
    .with_call_stack(call_stack.clone());

    // 7. Execute the target program.
    let result = WasmExecutor::execute(
        &bytecode,
        program_id,
        instruction_data,
        &mut host_state,
        program_cache,
    );

    // 8. Sync compute consumption back to the caller.
    *compute_remaining = host_state.compute_remaining;

    // 9. Propagate the call stack updates back (the inner execution may have
    //    added/removed entries during nested CPI).
    call_stack.clear();
    call_stack.extend(host_state.call_stack);

    result
        .map(|_| ())
        .map_err(|e| format!("CPI execution failed: {e}"))
}
