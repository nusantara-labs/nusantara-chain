//! Shared helper functions for native program processors.
//!
//! These extract the most duplicated patterns across processor files:
//! account-count validation, signer checks, borsh state I/O, and rent checks.

use borsh::{BorshDeserialize, BorshSerialize};
use nusantara_crypto::Hash;

use crate::error::RuntimeError;
use crate::sysvar_cache::SysvarCache;
use crate::transaction_context::TransactionContext;

/// Verify that `accounts` has at least `min` entries.
///
/// Replaces ~30 instances of `if accounts.len() < N { return Err(...) }`.
pub fn require_accounts(accounts: &[u8], min: usize, name: &str) -> Result<(), RuntimeError> {
    if accounts.len() < min {
        return Err(RuntimeError::InvalidInstructionData(format!(
            "{name} requires {min} accounts"
        )));
    }
    Ok(())
}

/// Verify the account at `idx` is a signer and return its address.
///
/// Replaces ~28 instances of the signer-check-and-extract pattern.
pub fn require_signer(ctx: &TransactionContext, idx: usize) -> Result<Hash, RuntimeError> {
    let acc = ctx.get_account(idx)?;
    if !acc.is_signer {
        return Err(RuntimeError::AccountNotSigner(idx));
    }
    Ok(*acc.address)
}

/// Deserialize borsh state from the account at `idx`.
///
/// Replaces ~30+ instances of `try_from_slice` with error mapping.
pub fn load_state<T: BorshDeserialize>(
    ctx: &TransactionContext,
    idx: usize,
) -> Result<T, RuntimeError> {
    let acc = ctx.get_account(idx)?;
    BorshDeserialize::deserialize(&mut acc.account.data.as_slice())
        .map_err(|e| RuntimeError::InvalidAccountData(e.to_string()))
}

/// Serialize borsh state and write it to the account at `idx`.
///
/// Replaces ~26 instances of `borsh::to_vec` + write-to-account.
pub fn save_state<T: BorshSerialize>(
    ctx: &mut TransactionContext,
    idx: usize,
    state: &T,
) -> Result<(), RuntimeError> {
    let data = borsh::to_vec(state).map_err(|e| RuntimeError::InvalidAccountData(e.to_string()))?;
    let acc = ctx.get_account_mut(idx)?;
    acc.account.data = data;
    Ok(())
}

/// Check that the account at `idx` meets rent exemption and return the balance.
///
/// Replaces ~5 instances of rent exemption checks.
pub fn check_rent_exempt(
    ctx: &TransactionContext,
    idx: usize,
    sysvars: &SysvarCache,
) -> Result<u64, RuntimeError> {
    let acc = ctx.get_account(idx)?;
    let min = sysvars.rent().minimum_balance(acc.account.data.len());
    if acc.account.lamports < min {
        return Err(RuntimeError::RentNotMet {
            needed: min,
            available: acc.account.lamports,
        });
    }
    Ok(acc.account.lamports)
}
