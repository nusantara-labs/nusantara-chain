use nusantara_core::program::STAKE_PROGRAM_ID;
use nusantara_stake_program::{Delegation, Meta, Stake, StakeStateV2};

use super::super::helpers::{load_state, require_accounts, require_signer};
use crate::error::RuntimeError;
use crate::sysvar_cache::SysvarCache;
use crate::transaction_context::TransactionContext;

pub(super) fn process_split(
    accounts: &[u8],
    lamports: u64,
    ctx: &mut TransactionContext,
    sysvars: &SysvarCache,
) -> Result<(), RuntimeError> {
    require_accounts(accounts, 3, "Split")?;
    let stake_idx = accounts[0] as usize;
    let split_idx = accounts[1] as usize;
    let staker_idx = accounts[2] as usize;

    if stake_idx == split_idx {
        return Err(RuntimeError::AccountIndexAliasing {
            idx_a: stake_idx,
            idx_b: split_idx,
        });
    }

    let staker_address = require_signer(ctx, staker_idx)?;

    // Source must actually be a stake-program-owned account; otherwise a crafted
    // foreign-owned account with valid-looking Borsh bytes could pass load_state.
    {
        let acc = ctx.get_account(stake_idx)?;
        if acc.account.owner != *STAKE_PROGRAM_ID {
            return Err(RuntimeError::AccountOwnerMismatch);
        }
    }

    let (meta, stake_opt) = {
        let state: StakeStateV2 = load_state(ctx, stake_idx)?;
        match state {
            StakeStateV2::Initialized(m) => (m, None),
            StakeStateV2::Stake(m, s) => (m, Some(s)),
            _ => {
                return Err(RuntimeError::InvalidAccountData(
                    "stake account not initialized".to_string(),
                ));
            }
        }
    };

    if meta.authorized.staker != staker_address {
        return Err(RuntimeError::AccountNotSigner(staker_idx));
    }

    // Destination must be a fresh, uninitialized account. Splitting into a live
    // stake account would silently overwrite its state.
    {
        let split_acc = ctx.get_account(split_idx)?;
        if !split_acc.account.data.is_empty() {
            return Err(RuntimeError::AccountAlreadyExists);
        }
    }

    let source_lamports = {
        let acc = ctx.get_account(stake_idx)?;
        acc.account.lamports
    };

    if lamports > source_lamports.saturating_sub(meta.rent_exempt_reserve) {
        return Err(RuntimeError::InsufficientFunds {
            needed: lamports,
            available: source_lamports.saturating_sub(meta.rent_exempt_reserve),
        });
    }

    // Check rent on split (destination) account.
    // Both source and destination store the same StakeStateV2 enum, so use the
    // source data length as the baseline when the destination is empty.
    let split_rent_exempt = {
        let split_acc = ctx.get_account(split_idx)?;
        let data_len = if split_acc.account.data.is_empty() {
            let src = ctx.get_account(stake_idx)?;
            src.account.data.len()
        } else {
            split_acc.account.data.len()
        };
        sysvars.rent().minimum_balance(data_len)
    };

    // The split amount must cover the destination's rent-exempt reserve.
    if lamports < split_rent_exempt {
        return Err(RuntimeError::RentNotMet {
            needed: split_rent_exempt,
            available: lamports,
        });
    }

    // Build split state
    let split_state = if let Some(ref original_stake) = stake_opt {
        let original_total = source_lamports.saturating_sub(meta.rent_exempt_reserve);
        let split_stake_amount = if original_total > 0 {
            (original_stake.delegation.stake as u128 * lamports as u128 / original_total as u128)
                as u64
        } else {
            0
        };

        let split_delegation = Delegation {
            voter_pubkey: original_stake.delegation.voter_pubkey,
            stake: split_stake_amount,
            activation_epoch: original_stake.delegation.activation_epoch,
            deactivation_epoch: original_stake.delegation.deactivation_epoch,
            warmup_cooldown_rate_bps: original_stake.delegation.warmup_cooldown_rate_bps,
        };

        StakeStateV2::Stake(
            Meta {
                rent_exempt_reserve: split_rent_exempt,
                authorized: meta.authorized.clone(),
                lockup: meta.lockup.clone(),
            },
            Stake {
                delegation: split_delegation,
                credits_observed: original_stake.credits_observed,
            },
        )
    } else {
        StakeStateV2::Initialized(Meta {
            rent_exempt_reserve: split_rent_exempt,
            authorized: meta.authorized.clone(),
            lockup: meta.lockup.clone(),
        })
    };

    let split_data =
        borsh::to_vec(&split_state).map_err(|e| RuntimeError::InvalidAccountData(e.to_string()))?;

    // Update source: reduce lamports and stake
    if let Some(mut original_stake) = stake_opt {
        let original_total = source_lamports.saturating_sub(meta.rent_exempt_reserve);
        let split_stake_amount = if original_total > 0 {
            (original_stake.delegation.stake as u128 * lamports as u128 / original_total as u128)
                as u64
        } else {
            0
        };
        original_stake.delegation.stake = original_stake
            .delegation
            .stake
            .checked_sub(split_stake_amount)
            .ok_or(RuntimeError::InsufficientFunds {
                needed: split_stake_amount,
                available: original_stake.delegation.stake,
            })?;

        let updated_source = StakeStateV2::Stake(meta, original_stake);
        let source_data = borsh::to_vec(&updated_source)
            .map_err(|e| RuntimeError::InvalidAccountData(e.to_string()))?;

        let acc = ctx.get_account_mut(stake_idx)?;
        acc.account.lamports = acc
            .account
            .lamports
            .checked_sub(lamports)
            .ok_or(RuntimeError::InsufficientFunds {
                needed: lamports,
                available: acc.account.lamports,
            })?;
        acc.account.data = source_data;
    } else {
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

    // Configure split account
    {
        let acc = ctx.get_account_mut(split_idx)?;
        acc.account.lamports = acc
            .account
            .lamports
            .checked_add(lamports)
            .ok_or(RuntimeError::LamportsOverflow)?;
        acc.account.data = split_data;
        // Explicitly set ownership so the new account is a valid stake account
        // regardless of what owner the caller pre-funded it with.
        acc.account.owner = *STAKE_PROGRAM_ID;
    }

    Ok(())
}
