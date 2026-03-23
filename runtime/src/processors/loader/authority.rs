use nusantara_loader_program::state::LoaderState;

use crate::error::RuntimeError;
use crate::processors::helpers::{require_accounts, require_signer};
use crate::transaction_context::TransactionContext;

pub(super) fn process_set_authority(
    accounts: &[u8],
    new_authority: Option<nusantara_crypto::Hash>,
    ctx: &mut TransactionContext,
) -> Result<(), RuntimeError> {
    require_accounts(accounts, 2, "SetAuthority")?;
    let account_idx = accounts[0] as usize;
    let current_authority_idx = accounts[1] as usize;

    let current_authority_address = require_signer(ctx, current_authority_idx)?;

    // Read current state, verify authority, write new state
    let current_state = {
        let acc = ctx.get_account(account_idx)?;
        LoaderState::from_account_data(&acc.account.data)
            .map_err(RuntimeError::InvalidAccountData)?
    };

    let new_state = match &current_state {
        LoaderState::Buffer { authority } => {
            if *authority != Some(current_authority_address) {
                return Err(RuntimeError::AccountNotSigner(current_authority_idx));
            }
            LoaderState::Buffer {
                authority: new_authority,
            }
        }
        LoaderState::ProgramData {
            slot,
            upgrade_authority,
            bytecode_len,
        } => {
            if *upgrade_authority != Some(current_authority_address) {
                return Err(RuntimeError::AccountNotSigner(current_authority_idx));
            }
            LoaderState::ProgramData {
                slot: *slot,
                upgrade_authority: new_authority,
                bytecode_len: *bytecode_len,
            }
        }
        _ => {
            return Err(RuntimeError::InvalidAccountData(
                "cannot set authority on this account type".to_string(),
            ));
        }
    };

    let new_state_bytes =
        borsh::to_vec(&new_state).map_err(|e| RuntimeError::InvalidAccountData(e.to_string()))?;

    // For ProgramData, preserve the bytecode after the header
    let acc = ctx.get_account_mut(account_idx)?;
    match &current_state {
        LoaderState::ProgramData { .. } => {
            // Keep bytecode intact, only update the header portion
            let old_header_bytes = borsh::to_vec(&current_state)
                .map_err(|e| RuntimeError::InvalidAccountData(e.to_string()))?;
            let bytecode_start = old_header_bytes.len();
            let bytecode = acc.account.data[bytecode_start..].to_vec();
            let mut new_data = new_state_bytes;
            new_data.extend_from_slice(&bytecode);
            acc.account.data = new_data;
        }
        _ => {
            acc.account.data = new_state_bytes;
        }
    }

    Ok(())
}

pub(super) fn process_close(
    accounts: &[u8],
    ctx: &mut TransactionContext,
) -> Result<(), RuntimeError> {
    require_accounts(accounts, 3, "Close")?;
    let close_idx = accounts[0] as usize;
    let recipient_idx = accounts[1] as usize;
    let authority_idx = accounts[2] as usize;

    let authority_address = require_signer(ctx, authority_idx)?;

    // Verify the account's authority matches
    let lamports_to_transfer = {
        let acc = ctx.get_account(close_idx)?;
        let state = LoaderState::from_account_data(&acc.account.data)
            .map_err(RuntimeError::InvalidAccountData)?;
        match state.authority() {
            Some(auth) if *auth == authority_address => {}
            _ => {
                return Err(RuntimeError::AccountNotSigner(authority_idx));
            }
        }
        acc.account.lamports
    };

    // Transfer lamports to recipient
    {
        let recipient = ctx.get_account_mut(recipient_idx)?;
        recipient.account.lamports = recipient
            .account
            .lamports
            .checked_add(lamports_to_transfer)
            .ok_or(RuntimeError::LamportsOverflow)?;
    }

    // Clear the closed account
    {
        let acc = ctx.get_account_mut(close_idx)?;
        acc.account.lamports = 0;
        acc.account.data.clear();
    }

    Ok(())
}
