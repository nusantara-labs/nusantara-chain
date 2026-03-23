use borsh::BorshDeserialize;
use nusantara_core::program::TOKEN_PROGRAM_ID;
use nusantara_token_program::error::TokenError;
use nusantara_token_program::state::{AccountState, Mint, TokenAccount};

use crate::error::RuntimeError;
use crate::transaction_context::TransactionContext;

pub(super) fn process_freeze_account(
    accounts: &[u8],
    ctx: &mut TransactionContext,
) -> Result<(), RuntimeError> {
    if accounts.len() < 3 {
        return Err(super::token_err(TokenError::MissingAccount));
    }
    let account_idx = accounts[0] as usize;
    let mint_idx = accounts[1] as usize;
    let auth_idx = accounts[2] as usize;

    let auth = ctx.get_account(auth_idx)?;
    if !auth.is_signer {
        return Err(super::token_err(TokenError::MissingSigner));
    }
    let auth_address = *auth.address;

    // Verify token account is owned by the token program
    let acc_check = ctx.get_account(account_idx)?;
    if acc_check.account.owner != *TOKEN_PROGRAM_ID {
        return Err(RuntimeError::AccountOwnerMismatch);
    }

    // Check mint has freeze authority
    let mint_acc = ctx.get_account(mint_idx)?;
    let mint = Mint::try_from_slice(&mint_acc.account.data)
        .map_err(|_| super::token_err(TokenError::NotInitialized))?;
    if mint.freeze_authority != Some(auth_address) {
        return Err(super::token_err(TokenError::NoFreezeAuthority));
    }

    let acc = ctx.get_account(account_idx)?;
    let mut token_acc = TokenAccount::try_from_slice(&acc.account.data)
        .map_err(|_| super::token_err(TokenError::NotInitialized))?;

    let mint_address = *ctx.get_account(mint_idx)?.address;
    if token_acc.mint != mint_address {
        return Err(super::token_err(TokenError::MintMismatch));
    }

    token_acc.state = AccountState::Frozen;

    let acc_data = borsh::to_vec(&token_acc).map_err(super::borsh_err)?;
    ctx.get_account_mut(account_idx)?.account.data = acc_data;

    Ok(())
}

pub(super) fn process_thaw_account(
    accounts: &[u8],
    ctx: &mut TransactionContext,
) -> Result<(), RuntimeError> {
    if accounts.len() < 3 {
        return Err(super::token_err(TokenError::MissingAccount));
    }
    let account_idx = accounts[0] as usize;
    let mint_idx = accounts[1] as usize;
    let auth_idx = accounts[2] as usize;

    let auth = ctx.get_account(auth_idx)?;
    if !auth.is_signer {
        return Err(super::token_err(TokenError::MissingSigner));
    }
    let auth_address = *auth.address;

    // Verify token account is owned by the token program
    let acc_check = ctx.get_account(account_idx)?;
    if acc_check.account.owner != *TOKEN_PROGRAM_ID {
        return Err(RuntimeError::AccountOwnerMismatch);
    }

    let mint_acc = ctx.get_account(mint_idx)?;
    let mint = Mint::try_from_slice(&mint_acc.account.data)
        .map_err(|_| super::token_err(TokenError::NotInitialized))?;
    if mint.freeze_authority != Some(auth_address) {
        return Err(super::token_err(TokenError::NoFreezeAuthority));
    }

    let acc = ctx.get_account(account_idx)?;
    let mut token_acc = TokenAccount::try_from_slice(&acc.account.data)
        .map_err(|_| super::token_err(TokenError::NotInitialized))?;

    let mint_address = *ctx.get_account(mint_idx)?.address;
    if token_acc.mint != mint_address {
        return Err(super::token_err(TokenError::MintMismatch));
    }

    if token_acc.state != AccountState::Frozen {
        return Err(super::token_err(TokenError::NotInitialized));
    }

    token_acc.state = AccountState::Initialized;

    let acc_data = borsh::to_vec(&token_acc).map_err(super::borsh_err)?;
    ctx.get_account_mut(account_idx)?.account.data = acc_data;

    Ok(())
}
