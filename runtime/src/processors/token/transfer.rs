use borsh::BorshDeserialize;
use nusantara_core::program::TOKEN_PROGRAM_ID;
use nusantara_token_program::error::TokenError;
use nusantara_token_program::state::{AccountState, TokenAccount};

use crate::error::RuntimeError;
use crate::transaction_context::TransactionContext;

pub(super) fn process_transfer(
    accounts: &[u8],
    ctx: &mut TransactionContext,
    amount: u64,
) -> Result<(), RuntimeError> {
    if accounts.len() < 3 {
        return Err(super::token_err(TokenError::MissingAccount));
    }
    let src_idx = accounts[0] as usize;
    let dest_idx = accounts[1] as usize;
    let auth_idx = accounts[2] as usize;

    if src_idx == dest_idx {
        return Err(RuntimeError::AccountIndexAliasing {
            idx_a: src_idx,
            idx_b: dest_idx,
        });
    }

    let auth = ctx.get_account(auth_idx)?;
    if !auth.is_signer {
        return Err(super::token_err(TokenError::MissingSigner));
    }
    let auth_address = *auth.address;

    // Verify source account is owned by the token program
    let src_acc = ctx.get_account(src_idx)?;
    if src_acc.account.owner != *TOKEN_PROGRAM_ID {
        return Err(RuntimeError::AccountOwnerMismatch);
    }

    // Verify destination account is owned by the token program
    let dest_owner_check = ctx.get_account(dest_idx)?;
    if dest_owner_check.account.owner != *TOKEN_PROGRAM_ID {
        return Err(RuntimeError::AccountOwnerMismatch);
    }

    // Load source
    let src_acc = ctx.get_account(src_idx)?;
    let mut src_token = TokenAccount::try_from_slice(&src_acc.account.data)
        .map_err(|_| super::token_err(TokenError::NotInitialized))?;
    if src_token.state == AccountState::Frozen {
        return Err(super::token_err(TokenError::AccountFrozen));
    }

    // Check authority: must be owner or delegate
    let is_delegate = src_token.delegate == Some(auth_address) && src_token.delegated_amount > 0;
    if src_token.owner != auth_address && !is_delegate {
        return Err(super::token_err(TokenError::OwnerMismatch));
    }

    if src_token.amount < amount {
        return Err(super::token_err(TokenError::InsufficientBalance {
            need: amount,
            have: src_token.amount,
        }));
    }

    if is_delegate {
        if src_token.delegated_amount < amount {
            return Err(super::token_err(TokenError::InsufficientDelegation {
                need: amount,
                have: src_token.delegated_amount,
            }));
        }
        src_token.delegated_amount -= amount;
    }

    // Load destination
    let dest_acc = ctx.get_account(dest_idx)?;
    let mut dest_token = TokenAccount::try_from_slice(&dest_acc.account.data)
        .map_err(|_| super::token_err(TokenError::NotInitialized))?;
    if dest_token.state == AccountState::Frozen {
        return Err(super::token_err(TokenError::AccountFrozen));
    }
    if src_token.mint != dest_token.mint {
        return Err(super::token_err(TokenError::MintMismatch));
    }

    src_token.amount -= amount;
    dest_token.amount = dest_token
        .amount
        .checked_add(amount)
        .ok_or(super::token_err(TokenError::SupplyOverflow))?;

    // Write back
    let src_data = borsh::to_vec(&src_token).map_err(super::borsh_err)?;
    ctx.get_account_mut(src_idx)?.account.data = src_data;

    let dest_data = borsh::to_vec(&dest_token).map_err(super::borsh_err)?;
    ctx.get_account_mut(dest_idx)?.account.data = dest_data;

    Ok(())
}

pub(super) fn process_approve(
    accounts: &[u8],
    ctx: &mut TransactionContext,
    amount: u64,
) -> Result<(), RuntimeError> {
    if accounts.len() < 3 {
        return Err(super::token_err(TokenError::MissingAccount));
    }
    let src_idx = accounts[0] as usize;
    let delegate_idx = accounts[1] as usize;
    let owner_idx = accounts[2] as usize;

    let owner = ctx.get_account(owner_idx)?;
    if !owner.is_signer {
        return Err(super::token_err(TokenError::MissingSigner));
    }
    let owner_address = *owner.address;

    let delegate_address = *ctx.get_account(delegate_idx)?.address;

    let src_acc = ctx.get_account(src_idx)?;
    let mut token_acc = TokenAccount::try_from_slice(&src_acc.account.data)
        .map_err(|_| super::token_err(TokenError::NotInitialized))?;

    if token_acc.owner != owner_address {
        return Err(super::token_err(TokenError::OwnerMismatch));
    }

    token_acc.delegate = Some(delegate_address);
    token_acc.delegated_amount = amount;

    let acc_data = borsh::to_vec(&token_acc).map_err(super::borsh_err)?;
    ctx.get_account_mut(src_idx)?.account.data = acc_data;

    Ok(())
}

pub(super) fn process_revoke(
    accounts: &[u8],
    ctx: &mut TransactionContext,
) -> Result<(), RuntimeError> {
    if accounts.len() < 2 {
        return Err(super::token_err(TokenError::MissingAccount));
    }
    let src_idx = accounts[0] as usize;
    let owner_idx = accounts[1] as usize;

    let owner = ctx.get_account(owner_idx)?;
    if !owner.is_signer {
        return Err(super::token_err(TokenError::MissingSigner));
    }
    let owner_address = *owner.address;

    let src_acc = ctx.get_account(src_idx)?;
    let mut token_acc = TokenAccount::try_from_slice(&src_acc.account.data)
        .map_err(|_| super::token_err(TokenError::NotInitialized))?;

    if token_acc.owner != owner_address {
        return Err(super::token_err(TokenError::OwnerMismatch));
    }

    token_acc.delegate = None;
    token_acc.delegated_amount = 0;

    let acc_data = borsh::to_vec(&token_acc).map_err(super::borsh_err)?;
    ctx.get_account_mut(src_idx)?.account.data = acc_data;

    Ok(())
}
