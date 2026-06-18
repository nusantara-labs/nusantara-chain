use borsh::BorshDeserialize;
use nusantara_core::program::{SYSTEM_PROGRAM_ID, TOKEN_PROGRAM_ID};
use nusantara_token_program::error::TokenError;
use nusantara_token_program::state::Mint;

use crate::error::RuntimeError;
use crate::transaction_context::TransactionContext;

pub(super) fn process_initialize_mint(
    accounts: &[u8],
    ctx: &mut TransactionContext,
    decimals: u8,
    mint_authority: nusantara_crypto::Hash,
    freeze_authority: Option<nusantara_crypto::Hash>,
) -> Result<(), RuntimeError> {
    if accounts.is_empty() {
        return Err(super::token_err(TokenError::MissingAccount));
    }
    let mint_idx = accounts[0] as usize;

    // Check if already initialized
    let existing = ctx.get_account(mint_idx)?;
    // Reject if account is owned by another program (prevents hijacking)
    if existing.account.owner != nusantara_crypto::Hash::zero()
        && existing.account.owner != *SYSTEM_PROGRAM_ID
        && existing.account.owner != *TOKEN_PROGRAM_ID
    {
        return Err(RuntimeError::AccountOwnerMismatch);
    }
    if !existing.account.data.is_empty()
        && let Ok(m) = Mint::deserialize(&mut existing.account.data.as_slice())
        && m.is_initialized
    {
        return Err(super::token_err(TokenError::AlreadyInitialized));
    }

    let mint = Mint {
        mint_authority: Some(mint_authority),
        supply: 0,
        decimals,
        is_initialized: true,
        freeze_authority,
    };

    let mint_data = borsh::to_vec(&mint).map_err(super::borsh_err)?;

    {
        let acc = ctx.get_account_mut(mint_idx)?;
        acc.account.data = mint_data;
        acc.account.owner = *TOKEN_PROGRAM_ID;
    }

    Ok(())
}

pub(super) fn process_mint_to(
    accounts: &[u8],
    ctx: &mut TransactionContext,
    amount: u64,
) -> Result<(), RuntimeError> {
    if accounts.len() < 3 {
        return Err(super::token_err(TokenError::MissingAccount));
    }
    let mint_idx = accounts[0] as usize;
    let dest_idx = accounts[1] as usize;
    let auth_idx = accounts[2] as usize;

    // Verify authority is signer
    let auth = ctx.get_account(auth_idx)?;
    if !auth.is_signer {
        return Err(super::token_err(TokenError::MissingSigner));
    }
    let auth_address = *auth.address;

    // Load and update mint
    let mint_acc = ctx.get_account(mint_idx)?;
    let mut mint = Mint::deserialize(&mut mint_acc.account.data.as_slice())
        .map_err(|_| super::token_err(TokenError::NotInitialized))?;
    if !mint.is_initialized {
        return Err(super::token_err(TokenError::NotInitialized));
    }
    if mint.mint_authority != Some(auth_address) {
        return Err(super::token_err(TokenError::AuthorityMismatch));
    }
    mint.supply = mint
        .supply
        .checked_add(amount)
        .ok_or(super::token_err(TokenError::SupplyOverflow))?;

    // Verify destination account is owned by the token program
    use nusantara_token_program::state::{AccountState, TokenAccount};
    let dest_acc = ctx.get_account(dest_idx)?;
    if dest_acc.account.owner != *TOKEN_PROGRAM_ID {
        return Err(RuntimeError::AccountOwnerMismatch);
    }

    // Load and update destination token account
    let dest_acc = ctx.get_account(dest_idx)?;
    let mut token_acc = TokenAccount::deserialize(&mut dest_acc.account.data.as_slice())
        .map_err(|_| super::token_err(TokenError::NotInitialized))?;
    if token_acc.state == AccountState::Frozen {
        return Err(super::token_err(TokenError::AccountFrozen));
    }
    let mint_address = *ctx.get_account(mint_idx)?.address;
    if token_acc.mint != mint_address {
        return Err(super::token_err(TokenError::MintMismatch));
    }
    token_acc.amount = token_acc
        .amount
        .checked_add(amount)
        .ok_or(super::token_err(TokenError::SupplyOverflow))?;

    // Write back
    let mint_data = borsh::to_vec(&mint).map_err(super::borsh_err)?;
    ctx.get_account_mut(mint_idx)?.account.data = mint_data;

    let acc_data = borsh::to_vec(&token_acc).map_err(super::borsh_err)?;
    ctx.get_account_mut(dest_idx)?.account.data = acc_data;

    Ok(())
}
