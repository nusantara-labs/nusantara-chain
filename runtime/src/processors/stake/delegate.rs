use borsh::BorshDeserialize;
use nusantara_stake_program::{
    DEFAULT_MIN_DELEGATION, DEFAULT_WARMUP_COOLDOWN_RATE_BPS, Delegation, Stake, StakeStateV2,
};

use super::super::helpers::{require_accounts, require_signer, save_state};
use crate::error::RuntimeError;
use crate::sysvar_cache::SysvarCache;
use crate::transaction_context::TransactionContext;

pub(super) fn process_delegate(
    accounts: &[u8],
    ctx: &mut TransactionContext,
    sysvars: &SysvarCache,
) -> Result<(), RuntimeError> {
    require_accounts(accounts, 3, "DelegateStake")?;
    let stake_idx = accounts[0] as usize;
    let vote_idx = accounts[1] as usize;
    let staker_idx = accounts[2] as usize;

    let staker_address = require_signer(ctx, staker_idx)?;

    let vote_address = {
        let vote = ctx.get_account(vote_idx)?;
        *vote.address
    };

    // Load current stake state
    let (meta, _current_state) = {
        let acc = ctx.get_account(stake_idx)?;
        let state = StakeStateV2::try_from_slice(&acc.account.data)
            .map_err(|e| RuntimeError::InvalidAccountData(e.to_string()))?;
        match state {
            StakeStateV2::Initialized(m) => (m, "initialized"),
            _ => {
                return Err(RuntimeError::InvalidAccountData(
                    "stake account must be initialized to delegate".to_string(),
                ));
            }
        }
    };

    // Verify staker authorization
    if meta.authorized.staker != staker_address {
        return Err(RuntimeError::AccountNotSigner(staker_idx));
    }

    let stake_lamports = {
        let acc = ctx.get_account(stake_idx)?;
        acc.account
            .lamports
            .saturating_sub(meta.rent_exempt_reserve)
    };

    if stake_lamports < DEFAULT_MIN_DELEGATION {
        return Err(RuntimeError::InsufficientFunds {
            needed: DEFAULT_MIN_DELEGATION,
            available: stake_lamports,
        });
    }

    let delegation = Delegation {
        voter_pubkey: vote_address,
        stake: stake_lamports,
        activation_epoch: sysvars.clock().epoch,
        deactivation_epoch: u64::MAX,
        warmup_cooldown_rate_bps: DEFAULT_WARMUP_COOLDOWN_RATE_BPS,
    };

    let stake = Stake {
        delegation,
        credits_observed: 0,
    };

    let new_state = StakeStateV2::Stake(meta, stake);
    save_state(ctx, stake_idx, &new_state)
}
