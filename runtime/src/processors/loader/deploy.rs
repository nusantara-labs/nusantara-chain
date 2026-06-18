use nusantara_core::MAX_ACCOUNT_DATA_SIZE;
use nusantara_core::program::{LOADER_PROGRAM_ID, SYSTEM_PROGRAM_ID};
use nusantara_crypto::Hash;
use nusantara_loader_program::state::LoaderState;
use nusantara_vm::validate_wasm;

use crate::error::RuntimeError;
use crate::processors::helpers::{require_accounts, require_signer};
use crate::sysvar_cache::SysvarCache;
use crate::transaction_context::TransactionContext;

pub(super) fn process_deploy(
    accounts: &[u8],
    max_data_len: u64,
    ctx: &mut TransactionContext,
    sysvars: &SysvarCache,
) -> Result<(), RuntimeError> {
    require_accounts(accounts, 5, "Deploy")?;
    let payer_idx = accounts[0] as usize;
    let program_idx = accounts[1] as usize;
    let program_data_idx = accounts[2] as usize;
    let buffer_idx = accounts[3] as usize;
    let authority_idx = accounts[4] as usize;

    require_signer(ctx, payer_idx)?;
    // The program account must be a signer so the caller explicitly controls
    // which address becomes the executable.
    require_signer(ctx, program_idx)?;
    let authority_address = require_signer(ctx, authority_idx)?;

    // Verify both target accounts are completely uninitialized (empty data,
    // not executable, owned by system). Writing into a live program or
    // program-data account would silently corrupt it.
    // Fresh-account check: empty data and not executable. Owner may be Hash::zero
    // (unloaded/missing) or SYSTEM_PROGRAM_ID; either is acceptable for a brand
    // new account that has never been written to.
    {
        let program = ctx.get_account(program_idx)?;
        if !program.account.data.is_empty()
            || program.account.executable
            || program.account.lamports != 0
            || (program.account.owner != *SYSTEM_PROGRAM_ID
                && program.account.owner != Hash::zero())
        {
            return Err(RuntimeError::AccountAlreadyExists);
        }
    }
    {
        let pd = ctx.get_account(program_data_idx)?;
        // PD must additionally have zero lamports — otherwise crediting pd_rent
        // via checked_add would violate lamport conservation (payer pays pd_rent
        // but PD's total grows by pd_rent + pre-existing balance).
        if !pd.account.data.is_empty()
            || pd.account.executable
            || pd.account.lamports != 0
            || (pd.account.owner != *SYSTEM_PROGRAM_ID && pd.account.owner != Hash::zero())
        {
            return Err(RuntimeError::AccountAlreadyExists);
        }
    }

    // Extract bytecode from buffer
    let (bytecode, buffer_lamports) = {
        let buffer = ctx.get_account(buffer_idx)?;
        if buffer.account.owner != *LOADER_PROGRAM_ID {
            return Err(RuntimeError::AccountOwnerMismatch);
        }
        let state = LoaderState::from_account_data(&buffer.account.data)
            .map_err(RuntimeError::InvalidAccountData)?;
        match &state {
            LoaderState::Buffer { authority } => {
                if *authority != Some(authority_address) {
                    return Err(RuntimeError::AccountNotSigner(authority_idx));
                }
            }
            _ => {
                return Err(RuntimeError::InvalidAccountData(
                    "account is not a buffer".to_string(),
                ));
            }
        }
        // Extract bytecode (everything after the header)
        let header_bytes =
            borsh::to_vec(&state).map_err(|e| RuntimeError::InvalidAccountData(e.to_string()))?;
        let bytecode = buffer.account.data[header_bytes.len()..].to_vec();
        (bytecode, buffer.account.lamports)
    };

    // Validate WASM bytecode
    validate_wasm(&bytecode).map_err(|e| RuntimeError::WasmError(e.to_string()))?;

    // Get program_data_address for the Program account to point to
    let program_data_address = {
        let pd = ctx.get_account(program_data_idx)?;
        *pd.address
    };

    // Create Program account (executable = true, owner = LOADER_PROGRAM_ID)
    let program_state = LoaderState::Program {
        program_data_address,
    };
    let program_state_bytes = borsh::to_vec(&program_state)
        .map_err(|e| RuntimeError::InvalidAccountData(e.to_string()))?;

    {
        let program = ctx.get_account_mut(program_idx)?;
        program.account.owner = *LOADER_PROGRAM_ID;
        program.account.executable = true;
        program.account.data = program_state_bytes;
    }

    // Create ProgramData account (header + bytecode, with max_data_len padding)
    let pd_header = LoaderState::ProgramData {
        slot: ctx.slot,
        upgrade_authority: Some(authority_address),
        bytecode_len: bytecode.len() as u64,
    };
    let pd_header_bytes =
        borsh::to_vec(&pd_header).map_err(|e| RuntimeError::InvalidAccountData(e.to_string()))?;

    // Guard against unbounded allocation
    if max_data_len > MAX_ACCOUNT_DATA_SIZE {
        return Err(RuntimeError::AccountDataTooLarge {
            size: max_data_len,
            limit: MAX_ACCOUNT_DATA_SIZE,
        });
    }

    let bytecode_space = max_data_len.max(bytecode.len() as u64) as usize;
    let total_pd_size = pd_header_bytes.len() + bytecode_space;

    // Calculate rent for ProgramData
    let pd_rent = sysvars.rent().minimum_balance(total_pd_size);

    // Deduct rent from payer
    {
        let payer = ctx.get_account_mut(payer_idx)?;
        payer.account.lamports = payer.account.lamports.checked_sub(pd_rent).ok_or(
            RuntimeError::InsufficientFunds {
                needed: pd_rent,
                available: payer.account.lamports,
            },
        )?;
    }

    // Write ProgramData account
    {
        let pd = ctx.get_account_mut(program_data_idx)?;
        pd.account.owner = *LOADER_PROGRAM_ID;
        // Use checked_add so any pre-existing lamports are preserved rather than
        // silently overwritten, and so arithmetic overflow is caught cleanly.
        pd.account.lamports = pd
            .account
            .lamports
            .checked_add(pd_rent)
            .ok_or(RuntimeError::LamportsOverflow)?;
        let mut pd_data = pd_header_bytes;
        pd_data.extend_from_slice(&bytecode);
        pd_data.resize(pd_data.len() + bytecode_space - bytecode.len(), 0);
        pd.account.data = pd_data;
    }

    // Close buffer: transfer lamports to payer, clear data
    {
        let buffer = ctx.get_account_mut(buffer_idx)?;
        buffer.account.data.clear();
        buffer.account.lamports = 0;
    }
    {
        let payer = ctx.get_account_mut(payer_idx)?;
        payer.account.lamports = payer
            .account
            .lamports
            .checked_add(buffer_lamports)
            .ok_or(RuntimeError::LamportsOverflow)?;
    }

    Ok(())
}
