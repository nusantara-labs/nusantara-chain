use borsh::BorshDeserialize;
use nusantara_stake_program::{Authorized, Lockup, Meta, StakeStateV2};

use nusantara_core::program::STAKE_PROGRAM_ID;

use super::super::helpers::{require_accounts, save_state};
use crate::error::RuntimeError;
use crate::sysvar_cache::SysvarCache;
use crate::transaction_context::TransactionContext;

pub(super) fn process_initialize(
    accounts: &[u8],
    authorized: Authorized,
    lockup: Lockup,
    ctx: &mut TransactionContext,
    sysvars: &SysvarCache,
) -> Result<(), RuntimeError> {
    require_accounts(accounts, 1, "Initialize")?;
    let stake_idx = accounts[0] as usize;

    // Verify account owner
    {
        let acc = ctx.get_account(stake_idx)?;
        if acc.account.owner != *STAKE_PROGRAM_ID {
            return Err(RuntimeError::AccountOwnerMismatch);
        }
    }

    // Check current state
    let current_state = {
        let acc = ctx.get_account(stake_idx)?;
        if acc.account.data.is_empty() {
            StakeStateV2::Uninitialized
        } else {
            BorshDeserialize::deserialize(&mut acc.account.data.as_slice())
                .map_err(|e| RuntimeError::InvalidAccountData(e.to_string()))?
        }
    };

    if current_state != StakeStateV2::Uninitialized {
        return Err(RuntimeError::InvalidAccountData(
            "stake account already initialized".to_string(),
        ));
    }

    // Check rent exemption
    let rent_exempt_reserve = {
        let acc = ctx.get_account(stake_idx)?;
        let reserve = sysvars.rent().minimum_balance(acc.account.data.len());
        if acc.account.lamports < reserve {
            return Err(RuntimeError::RentNotMet {
                needed: reserve,
                available: acc.account.lamports,
            });
        }
        reserve
    };

    let meta = Meta {
        rent_exempt_reserve,
        authorized,
        lockup,
    };
    let state = StakeStateV2::Initialized(meta);
    save_state(ctx, stake_idx, &state)
}
