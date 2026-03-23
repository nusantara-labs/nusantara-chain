use nusantara_vote_program::VoteState;

use crate::error::RuntimeError;
use crate::processors::helpers::{load_state, require_accounts, require_signer, save_state};
use crate::transaction_context::TransactionContext;

pub(super) fn process_update_commission(
    accounts: &[u8],
    commission: u8,
    ctx: &mut TransactionContext,
) -> Result<(), RuntimeError> {
    require_accounts(accounts, 2, "UpdateCommission")?;
    let vote_idx = accounts[0] as usize;
    let auth_idx = accounts[1] as usize;

    let auth_address = require_signer(ctx, auth_idx)?;

    if commission > 100 {
        return Err(RuntimeError::ProgramError {
            program: "vote".to_string(),
            message: format!("commission {commission} exceeds 100%"),
        });
    }

    let mut state: VoteState = load_state(ctx, vote_idx)?;

    if state.authorized_withdrawer != auth_address {
        return Err(RuntimeError::ProgramError {
            program: "vote".to_string(),
            message: "not authorized withdrawer".to_string(),
        });
    }

    state.commission = commission;
    save_state(ctx, vote_idx, &state)
}
