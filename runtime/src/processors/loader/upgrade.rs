use nusantara_loader_program::state::LoaderState;
use nusantara_vm::validate_wasm;

use crate::error::RuntimeError;
use crate::processors::helpers::{require_accounts, require_signer};
use crate::transaction_context::TransactionContext;

pub(super) fn process_upgrade(
    accounts: &[u8],
    ctx: &mut TransactionContext,
) -> Result<(), RuntimeError> {
    require_accounts(accounts, 4, "Upgrade")?;
    let program_idx = accounts[0] as usize;
    let program_data_idx = accounts[1] as usize;
    let buffer_idx = accounts[2] as usize;
    let authority_idx = accounts[3] as usize;

    let authority_address = require_signer(ctx, authority_idx)?;

    // Verify program account points to program_data
    let program_data_address = {
        let pd = ctx.get_account(program_data_idx)?;
        *pd.address
    };
    {
        let program = ctx.get_account(program_idx)?;
        let state = LoaderState::from_account_data(&program.account.data)
            .map_err(RuntimeError::InvalidAccountData)?;
        match state {
            LoaderState::Program {
                program_data_address: pda,
            } => {
                if pda != program_data_address {
                    return Err(RuntimeError::InvalidAccountData(
                        "program data address mismatch".to_string(),
                    ));
                }
            }
            _ => {
                return Err(RuntimeError::InvalidAccountData(
                    "not a program account".to_string(),
                ));
            }
        }
    }

    // Verify ProgramData authority matches
    let old_pd_header_len = {
        let pd = ctx.get_account(program_data_idx)?;
        let state = LoaderState::from_account_data(&pd.account.data)
            .map_err(RuntimeError::InvalidAccountData)?;
        match &state {
            LoaderState::ProgramData {
                upgrade_authority, ..
            } => {
                if upgrade_authority.is_none() {
                    return Err(RuntimeError::ProgramError {
                        program: "loader".to_string(),
                        message: "program is immutable".to_string(),
                    });
                }
                if *upgrade_authority != Some(authority_address) {
                    return Err(RuntimeError::AccountNotSigner(authority_idx));
                }
            }
            _ => {
                return Err(RuntimeError::InvalidAccountData(
                    "not a program data account".to_string(),
                ));
            }
        }
        borsh::to_vec(&state)
            .map_err(|e| RuntimeError::InvalidAccountData(e.to_string()))?
            .len()
    };

    // Extract new bytecode from buffer
    let (new_bytecode, buffer_lamports) = {
        let buffer = ctx.get_account(buffer_idx)?;
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
                    "not a buffer account".to_string(),
                ));
            }
        }
        let header_bytes =
            borsh::to_vec(&state).map_err(|e| RuntimeError::InvalidAccountData(e.to_string()))?;
        let bytecode = buffer.account.data[header_bytes.len()..].to_vec();
        (bytecode, buffer.account.lamports)
    };

    // Validate new WASM bytecode
    validate_wasm(&new_bytecode).map_err(|e| RuntimeError::WasmError(e.to_string()))?;

    // Update ProgramData with new bytecode
    let new_header = LoaderState::ProgramData {
        slot: ctx.slot,
        upgrade_authority: Some(authority_address),
        bytecode_len: new_bytecode.len() as u64,
    };
    let new_header_bytes =
        borsh::to_vec(&new_header).map_err(|e| RuntimeError::InvalidAccountData(e.to_string()))?;

    {
        let pd = ctx.get_account_mut(program_data_idx)?;
        // old_pd_header_len is the serialised byte length of the previous
        // ProgramData header; it must be <= the current data length or the
        // on-chain account is corrupt.
        let old_bytecode_space = pd
            .account
            .data
            .len()
            .checked_sub(old_pd_header_len)
            .ok_or(RuntimeError::InvalidAccountData(
                "program data account too small for its own header".to_string(),
            ))?;
        if new_bytecode.len() > old_bytecode_space {
            return Err(RuntimeError::AccountDataTooLarge {
                size: new_bytecode.len() as u64,
                limit: old_bytecode_space as u64,
            });
        }
        // Padding = (old capacity) - (new bytecode length); safe because the
        // guard above ensures new_bytecode.len() <= old_bytecode_space.
        let padding = old_bytecode_space
            .checked_sub(new_bytecode.len())
            .ok_or(RuntimeError::InvalidAccountData(
                "bytecode length exceeds allocated space".to_string(),
            ))?;
        let mut new_data = new_header_bytes;
        new_data.extend_from_slice(&new_bytecode);
        new_data.resize(new_data.len() + padding, 0);
        pd.account.data = new_data;
    }

    // Close buffer: return lamports to authority
    {
        let buffer = ctx.get_account_mut(buffer_idx)?;
        buffer.account.data.clear();
        buffer.account.lamports = 0;
    }
    {
        let auth = ctx.get_account_mut(authority_idx)?;
        auth.account.lamports = auth
            .account
            .lamports
            .checked_add(buffer_lamports)
            .ok_or(RuntimeError::LamportsOverflow)?;
    }

    Ok(())
}
