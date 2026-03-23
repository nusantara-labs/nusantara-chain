use nusantara_vote_program::VoteState;

use crate::error::RuntimeError;
use crate::processors::helpers::{load_state, require_accounts, require_signer};
use crate::sysvar_cache::SysvarCache;
use crate::transaction_context::TransactionContext;

pub(super) fn process_withdraw(
    accounts: &[u8],
    lamports: u64,
    ctx: &mut TransactionContext,
    sysvars: &SysvarCache,
) -> Result<(), RuntimeError> {
    require_accounts(accounts, 3, "Withdraw")?;
    let vote_idx = accounts[0] as usize;
    let to_idx = accounts[1] as usize;
    let withdrawer_idx = accounts[2] as usize;

    let withdrawer_address = require_signer(ctx, withdrawer_idx)?;

    let state: VoteState = load_state(ctx, vote_idx)?;

    if state.authorized_withdrawer != withdrawer_address {
        return Err(RuntimeError::ProgramError {
            program: "vote".to_string(),
            message: "not authorized withdrawer".to_string(),
        });
    }

    {
        let acc = ctx.get_account(vote_idx)?;
        if acc.account.lamports < lamports {
            return Err(RuntimeError::InsufficientFunds {
                needed: lamports,
                available: acc.account.lamports,
            });
        }
    }

    {
        let acc = ctx.get_account_mut(vote_idx)?;
        acc.account.lamports = acc
            .account
            .lamports
            .checked_sub(lamports)
            .ok_or(RuntimeError::InsufficientFunds {
                needed: lamports,
                available: acc.account.lamports,
            })?;
        // Allow full close (0 lamports) but reject partial withdrawals that
        // leave the account below rent-exempt minimum.
        if acc.account.lamports > 0 {
            let min = sysvars.rent().minimum_balance(acc.account.data.len());
            if acc.account.lamports < min {
                return Err(RuntimeError::RentNotMet {
                    needed: min,
                    available: acc.account.lamports,
                });
            }
        }
    }

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
