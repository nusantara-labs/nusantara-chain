use borsh::BorshDeserialize;
use nusantara_core::program::{SYSTEM_PROGRAM_ID, TOKEN_PROGRAM_ID};
use nusantara_crypto::Hash;
use nusantara_token_program::error::TokenError;
use nusantara_token_program::state::Mint;
use nusantara_token_program::state::{AccountState, TokenAccount};

use crate::error::RuntimeError;
use crate::transaction_context::TransactionContext;

pub(super) fn process_initialize_account(
    accounts: &[u8],
    ctx: &mut TransactionContext,
) -> Result<(), RuntimeError> {
    if accounts.len() < 3 {
        return Err(super::token_err(TokenError::MissingAccount));
    }
    let account_idx = accounts[0] as usize;
    let mint_idx = accounts[1] as usize;
    let owner_idx = accounts[2] as usize;

    // Verify mint is initialized
    let mint_acc = ctx.get_account(mint_idx)?;
    let mint = Mint::try_from_slice(&mint_acc.account.data)
        .map_err(|_| super::token_err(TokenError::NotInitialized))?;
    if !mint.is_initialized {
        return Err(super::token_err(TokenError::NotInitialized));
    }
    let mint_address = *mint_acc.address;

    let owner_address = *ctx.get_account(owner_idx)?.address;

    // Verify account is unowned (system/zero) before claiming it for the token program
    let existing = ctx.get_account(account_idx)?;
    let existing_owner = existing.account.owner;
    if existing_owner != *SYSTEM_PROGRAM_ID
        && existing_owner != Hash::zero()
        && existing_owner != *TOKEN_PROGRAM_ID
    {
        return Err(RuntimeError::AccountOwnerMismatch);
    }

    // Check not already initialized
    let existing = ctx.get_account(account_idx)?;
    if !existing.account.data.is_empty()
        && let Ok(ta) = TokenAccount::try_from_slice(&existing.account.data)
        && ta.state != AccountState::Uninitialized
    {
        return Err(super::token_err(TokenError::AlreadyInitialized));
    }

    let token_account = TokenAccount {
        mint: mint_address,
        owner: owner_address,
        amount: 0,
        delegate: None,
        state: AccountState::Initialized,
        delegated_amount: 0,
        close_authority: None,
    };

    let acc_data = borsh::to_vec(&token_account).map_err(super::borsh_err)?;

    {
        let acc = ctx.get_account_mut(account_idx)?;
        acc.account.data = acc_data;
        acc.account.owner = *TOKEN_PROGRAM_ID;
    }

    Ok(())
}

pub(super) fn process_close_account(
    accounts: &[u8],
    ctx: &mut TransactionContext,
) -> Result<(), RuntimeError> {
    if accounts.len() < 3 {
        return Err(super::token_err(TokenError::MissingAccount));
    }
    let account_idx = accounts[0] as usize;
    let dest_idx = accounts[1] as usize;
    let auth_idx = accounts[2] as usize;

    let auth = ctx.get_account(auth_idx)?;
    if !auth.is_signer {
        return Err(super::token_err(TokenError::MissingSigner));
    }
    let auth_address = *auth.address;

    let acc = ctx.get_account(account_idx)?;
    let token_acc = TokenAccount::try_from_slice(&acc.account.data)
        .map_err(|_| super::token_err(TokenError::NotInitialized))?;

    // Must be owner or close_authority
    let close_auth = token_acc.close_authority.unwrap_or(token_acc.owner);
    if close_auth != auth_address {
        return Err(super::token_err(TokenError::OwnerMismatch));
    }

    if token_acc.amount > 0 {
        return Err(super::token_err(TokenError::CloseNonEmpty));
    }

    // Transfer lamports to destination
    let lamports = acc.account.lamports;
    {
        let dest = ctx.get_account_mut(dest_idx)?;
        dest.account.lamports = dest
            .account
            .lamports
            .checked_add(lamports)
            .ok_or(RuntimeError::LamportsOverflow)?;
    }
    {
        let acc = ctx.get_account_mut(account_idx)?;
        acc.account.lamports = 0;
        acc.account.data.clear();
    }

    Ok(())
}
