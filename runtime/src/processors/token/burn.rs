use borsh::BorshDeserialize;
use nusantara_core::program::TOKEN_PROGRAM_ID;
use nusantara_token_program::error::TokenError;
use nusantara_token_program::state::{Mint, TokenAccount};

use crate::error::RuntimeError;
use crate::transaction_context::TransactionContext;

pub(super) fn process_burn(
    accounts: &[u8],
    ctx: &mut TransactionContext,
    amount: u64,
) -> Result<(), RuntimeError> {
    if accounts.len() < 3 {
        return Err(super::token_err(TokenError::MissingAccount));
    }
    let src_idx = accounts[0] as usize;
    let mint_idx = accounts[1] as usize;
    let auth_idx = accounts[2] as usize;

    if src_idx == mint_idx {
        return Err(RuntimeError::AccountIndexAliasing {
            idx_a: src_idx,
            idx_b: mint_idx,
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

    // Load source token account
    let src_acc = ctx.get_account(src_idx)?;
    let mut token_acc = TokenAccount::deserialize(&mut src_acc.account.data.as_slice())
        .map_err(|_| super::token_err(TokenError::NotInitialized))?;

    if token_acc.owner != auth_address && token_acc.delegate != Some(auth_address) {
        return Err(super::token_err(TokenError::OwnerMismatch));
    }

    if token_acc.amount < amount {
        return Err(super::token_err(TokenError::InsufficientBalance {
            need: amount,
            have: token_acc.amount,
        }));
    }

    let mint_address = *ctx.get_account(mint_idx)?.address;
    if token_acc.mint != mint_address {
        return Err(super::token_err(TokenError::MintMismatch));
    }

    token_acc.amount -= amount;

    // Update delegate if burning via delegation
    if token_acc.delegate == Some(auth_address) {
        if token_acc.delegated_amount < amount {
            return Err(super::token_err(TokenError::InsufficientDelegation {
                need: amount,
                have: token_acc.delegated_amount,
            }));
        }
        token_acc.delegated_amount -= amount;
    }

    // Update mint supply
    let mint_acc = ctx.get_account(mint_idx)?;
    let mut mint = Mint::deserialize(&mut mint_acc.account.data.as_slice())
        .map_err(|_| super::token_err(TokenError::NotInitialized))?;
    mint.supply = mint
        .supply
        .checked_sub(amount)
        .ok_or(super::token_err(TokenError::SupplyOverflow))?;

    // Write back
    let src_data = borsh::to_vec(&token_acc).map_err(super::borsh_err)?;
    ctx.get_account_mut(src_idx)?.account.data = src_data;

    let mint_data = borsh::to_vec(&mint).map_err(super::borsh_err)?;
    ctx.get_account_mut(mint_idx)?.account.data = mint_data;

    Ok(())
}
