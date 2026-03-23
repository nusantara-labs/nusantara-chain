use nusantara_vote_program::{Lockout, MAX_LOCKOUT_HISTORY, Vote, VoteState};

use crate::error::RuntimeError;
use crate::processors::helpers::{load_state, require_accounts, require_signer, save_state};
use crate::sysvar_cache::SysvarCache;
use crate::transaction_context::TransactionContext;

pub(super) fn process_vote_action(
    accounts: &[u8],
    vote: Vote,
    ctx: &mut TransactionContext,
    sysvars: &SysvarCache,
) -> Result<(), RuntimeError> {
    require_accounts(accounts, 2, "Vote")?;
    let vote_idx = accounts[0] as usize;
    let voter_idx = accounts[1] as usize;

    let voter_address = require_signer(ctx, voter_idx)?;

    let mut state: VoteState = load_state(ctx, vote_idx)?;

    // Verify authorization
    if state.authorized_voter != voter_address {
        return Err(RuntimeError::ProgramError {
            program: "vote".to_string(),
            message: "not authorized voter".to_string(),
        });
    }

    // Validate vote slots
    if vote.slots.is_empty() {
        return Err(RuntimeError::ProgramError {
            program: "vote".to_string(),
            message: "empty vote".to_string(),
        });
    }

    let current_slot = sysvars.clock().slot;
    for (i, &slot) in vote.slots.iter().enumerate() {
        if slot > current_slot {
            return Err(RuntimeError::ProgramError {
                program: "vote".to_string(),
                message: format!("vote slot {slot} is in the future (current: {current_slot})"),
            });
        }
        if i > 0 && slot <= vote.slots[i - 1] {
            return Err(RuntimeError::ProgramError {
                program: "vote".to_string(),
                message: "vote slots must be strictly ascending".to_string(),
            });
        }
    }

    // Process each vote slot
    for &slot in &vote.slots {
        // Add new lockout
        let lockout = Lockout {
            slot,
            confirmation_count: 1,
        };
        state.votes.push(lockout);

        // Increment confirmation counts for existing votes
        // that are not locked out at this slot
        let len = state.votes.len();
        if len > 1 {
            for i in (0..len - 1).rev() {
                state.votes[i].confirmation_count += 1;
            }
        }

        // Pop votes that have reached max lockout
        let excess = state
            .votes
            .len()
            .saturating_sub(MAX_LOCKOUT_HISTORY as usize);
        if excess > 0
            && let Some(oldest) = state.votes.drain(..excess).next_back()
        {
            state.root_slot = Some(oldest.slot);
        }
    }

    // Update epoch credits
    let current_epoch = sysvars.clock().epoch;
    if let Some(last) = state.epoch_credits.last_mut() {
        if last.0 == current_epoch {
            last.1 += vote.slots.len() as u64;
        } else {
            let prev_credits = last.1;
            state.epoch_credits.push((
                current_epoch,
                prev_credits + vote.slots.len() as u64,
                prev_credits,
            ));
        }
    } else {
        state
            .epoch_credits
            .push((current_epoch, vote.slots.len() as u64, 0));
    }

    // Update timestamp
    if let Some(ts) = vote.timestamp {
        state.last_timestamp = nusantara_vote_program::BlockTimestamp {
            slot: *vote.slots.last().unwrap_or(&0),
            timestamp: ts,
        };
    }

    save_state(ctx, vote_idx, &state)
}
