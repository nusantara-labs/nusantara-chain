use nusantara_core::program::STAKE_PROGRAM_ID;
use nusantara_stake_program::StakeStateV2;

use super::super::helpers::{load_state, require_accounts, require_signer, save_state};
use crate::error::RuntimeError;
use crate::sysvar_cache::SysvarCache;
use crate::transaction_context::TransactionContext;

pub(super) fn process_deactivate(
    accounts: &[u8],
    ctx: &mut TransactionContext,
    sysvars: &SysvarCache,
) -> Result<(), RuntimeError> {
    require_accounts(accounts, 2, "Deactivate")?;
    let stake_idx = accounts[0] as usize;
    let staker_idx = accounts[1] as usize;

    let staker_address = require_signer(ctx, staker_idx)?;

    // Verify ownership before touching state.
    {
        let acc = ctx.get_account(stake_idx)?;
        if acc.account.owner != *STAKE_PROGRAM_ID {
            return Err(RuntimeError::AccountOwnerMismatch);
        }
    }

    // Route through load_state which uses BorshDeserialize::deserialize to
    // tolerate trailing zeros in pre-allocated account data.
    let (meta, mut stake) = {
        let state: StakeStateV2 = load_state(ctx, stake_idx)?;
        match state {
            StakeStateV2::Stake(m, s) => (m, s),
            _ => {
                return Err(RuntimeError::InvalidAccountData(
                    "stake account must be delegated to deactivate".to_string(),
                ));
            }
        }
    };

    if meta.authorized.staker != staker_address {
        return Err(RuntimeError::AccountNotSigner(staker_idx));
    }

    if stake.delegation.deactivation_epoch != u64::MAX {
        return Err(RuntimeError::ProgramError {
            program: "stake".to_string(),
            message: "stake already deactivating".to_string(),
        });
    }

    stake.delegation.deactivation_epoch = sysvars.clock().epoch;

    let new_state = StakeStateV2::Stake(meta, stake);
    save_state(ctx, stake_idx, &new_state)
}
