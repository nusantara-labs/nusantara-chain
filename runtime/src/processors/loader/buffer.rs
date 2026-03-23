use nusantara_core::MAX_ACCOUNT_DATA_SIZE;
use nusantara_core::program::LOADER_PROGRAM_ID;
use nusantara_loader_program::state::LoaderState;

use crate::error::RuntimeError;
use crate::processors::helpers::{require_accounts, require_signer};
use crate::transaction_context::TransactionContext;

pub(super) fn process_initialize_buffer(
    accounts: &[u8],
    ctx: &mut TransactionContext,
) -> Result<(), RuntimeError> {
    require_accounts(accounts, 2, "InitializeBuffer")?;
    let buffer_idx = accounts[0] as usize;
    let authority_idx = accounts[1] as usize;

    let authority_address = require_signer(ctx, authority_idx)?;

    // Verify buffer is signer (new account)
    require_signer(ctx, buffer_idx)?;

    // Write buffer state
    let state = LoaderState::Buffer {
        authority: Some(authority_address),
    };
    let state_bytes =
        borsh::to_vec(&state).map_err(|e| RuntimeError::InvalidAccountData(e.to_string()))?;

    let buffer = ctx.get_account_mut(buffer_idx)?;
    buffer.account.owner = *LOADER_PROGRAM_ID;
    buffer.account.data = state_bytes;

    Ok(())
}

pub(super) fn process_write(
    accounts: &[u8],
    offset: u32,
    data: &[u8],
    ctx: &mut TransactionContext,
) -> Result<(), RuntimeError> {
    require_accounts(accounts, 2, "Write")?;
    let buffer_idx = accounts[0] as usize;
    let authority_idx = accounts[1] as usize;

    let authority_address = require_signer(ctx, authority_idx)?;

    // Verify buffer state and authority match
    {
        let buffer = ctx.get_account(buffer_idx)?;
        if buffer.account.owner != *LOADER_PROGRAM_ID {
            return Err(RuntimeError::AccountOwnerMismatch);
        }
        let state = LoaderState::from_account_data(&buffer.account.data)
            .map_err(RuntimeError::InvalidAccountData)?;
        match state {
            LoaderState::Buffer { authority } => {
                if authority != Some(authority_address) {
                    return Err(RuntimeError::AccountNotSigner(authority_idx));
                }
            }
            _ => {
                return Err(RuntimeError::InvalidAccountData(
                    "account is not a buffer".to_string(),
                ));
            }
        }
    }

    // Write data at offset. The buffer data layout is:
    // [LoaderState::Buffer header] ++ [raw bytecode bytes]
    // We need to ensure the data vec is large enough
    let buffer = ctx.get_account_mut(buffer_idx)?;
    let header_len = {
        let state = LoaderState::Buffer {
            authority: Some(authority_address),
        };
        borsh::to_vec(&state)
            .map_err(|e| RuntimeError::InvalidAccountData(e.to_string()))?
            .len()
    };

    let write_start = header_len + offset as usize;
    let write_end = write_start + data.len();

    // Guard against unbounded allocation
    if write_end as u64 > MAX_ACCOUNT_DATA_SIZE {
        return Err(RuntimeError::AccountDataTooLarge {
            size: write_end as u64,
            limit: MAX_ACCOUNT_DATA_SIZE,
        });
    }

    // Extend data if needed
    if write_end > buffer.account.data.len() {
        buffer.account.data.resize(write_end, 0);
    }

    buffer.account.data[write_start..write_end].copy_from_slice(data);

    Ok(())
}
