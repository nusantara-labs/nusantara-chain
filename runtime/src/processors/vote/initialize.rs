use borsh::BorshDeserialize;
use nusantara_vote_program::{VoteInit, VoteState};

use nusantara_core::program::VOTE_PROGRAM_ID;

use crate::error::RuntimeError;
use crate::processors::helpers::require_accounts;
use crate::sysvar_cache::SysvarCache;
use crate::transaction_context::TransactionContext;

pub(super) fn process_initialize(
    accounts: &[u8],
    init: VoteInit,
    ctx: &mut TransactionContext,
    sysvars: &SysvarCache,
) -> Result<(), RuntimeError> {
    require_accounts(accounts, 1, "InitializeAccount")?;
    let vote_idx = accounts[0] as usize;

    // Verify account owner
    {
        let acc = ctx.get_account(vote_idx)?;
        if acc.account.owner != *VOTE_PROGRAM_ID {
            return Err(RuntimeError::AccountOwnerMismatch);
        }
    }

    // Check if already initialized
    {
        let acc = ctx.get_account(vote_idx)?;
        if !acc.account.data.is_empty()
            && let Ok(state) = VoteState::try_from_slice(&acc.account.data)
            && (!state.votes.is_empty() || state.authorized_voter != nusantara_crypto::Hash::zero())
        {
            return Err(RuntimeError::InvalidAccountData(
                "vote account already initialized".to_string(),
            ));
        }
    }

    // Check rent exemption
    let state = VoteState::new(&init);
    let state_data =
        borsh::to_vec(&state).map_err(|e| RuntimeError::InvalidAccountData(e.to_string()))?;

    {
        let acc = ctx.get_account(vote_idx)?;
        let min = sysvars.rent().minimum_balance(state_data.len());
        if acc.account.lamports < min {
            return Err(RuntimeError::RentNotMet {
                needed: min,
                available: acc.account.lamports,
            });
        }
    }

    let acc = ctx.get_account_mut(vote_idx)?;
    acc.account.data = state_data;
    Ok(())
}
