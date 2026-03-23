use nusantara_stake_program::StakeStateV2;

use super::super::helpers::{load_state, require_accounts, require_signer};
use crate::error::RuntimeError;
use crate::sysvar_cache::SysvarCache;
use crate::transaction_context::TransactionContext;

pub(super) fn process_withdraw(
    accounts: &[u8],
    lamports: u64,
    ctx: &mut TransactionContext,
    sysvars: &SysvarCache,
) -> Result<(), RuntimeError> {
    require_accounts(accounts, 3, "Withdraw")?;
    let stake_idx = accounts[0] as usize;
    let to_idx = accounts[1] as usize;
    let withdrawer_idx = accounts[2] as usize;

    let withdrawer_address = require_signer(ctx, withdrawer_idx)?;

    let state: StakeStateV2 = load_state(ctx, stake_idx)?;

    let meta = match &state {
        StakeStateV2::Initialized(m) | StakeStateV2::Stake(m, _) => m,
        _ => {
            return Err(RuntimeError::InvalidAccountData(
                "stake account not initialized".to_string(),
            ));
        }
    };

    if meta.authorized.withdrawer != withdrawer_address {
        return Err(RuntimeError::AccountNotSigner(withdrawer_idx));
    }

    // Check lockup
    if meta.lockup.unix_timestamp > sysvars.clock().unix_timestamp
        || meta.lockup.epoch > sysvars.clock().epoch
    {
        return Err(RuntimeError::ProgramError {
            program: "stake".to_string(),
            message: "stake account is locked".to_string(),
        });
    }

    // Check available balance
    let available = {
        let acc = ctx.get_account(stake_idx)?;
        match &state {
            StakeStateV2::Initialized(_) => acc
                .account
                .lamports
                .saturating_sub(meta.rent_exempt_reserve),
            StakeStateV2::Stake(_, s) => {
                if s.delegation.deactivation_epoch >= sysvars.clock().epoch {
                    0 // still active or just deactivated this epoch
                } else {
                    let epochs_since =
                        sysvars.clock().epoch - s.delegation.deactivation_epoch;
                    let rate = s.delegation.warmup_cooldown_rate_bps as u128;
                    let cooled = ((s.delegation.stake as u128) * rate * (epochs_since as u128)
                        / 10_000)
                        .min(s.delegation.stake as u128)
                        as u64;
                    let still_staked = s.delegation.stake.saturating_sub(cooled);
                    acc.account
                        .lamports
                        .saturating_sub(meta.rent_exempt_reserve + still_staked)
                }
            }
            _ => 0,
        }
    };

    if lamports > available {
        return Err(RuntimeError::InsufficientFunds {
            needed: lamports,
            available,
        });
    }

    // Debit stake account
    {
        let acc = ctx.get_account_mut(stake_idx)?;
        acc.account.lamports = acc
            .account
            .lamports
            .checked_sub(lamports)
            .ok_or(RuntimeError::InsufficientFunds {
                needed: lamports,
                available: acc.account.lamports,
            })?;
    }

    // Credit destination
    {
        let acc = ctx.get_account_mut(to_idx)?;
        acc.account.lamports = acc
            .account
            .lamports
            .checked_add(lamports)
            .ok_or(RuntimeError::LamportsOverflow)?;
    }

    Ok(())
}
