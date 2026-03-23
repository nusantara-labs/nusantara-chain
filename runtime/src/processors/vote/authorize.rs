use nusantara_vote_program::{VoteAuthorize, VoteState};

use crate::error::RuntimeError;
use crate::processors::helpers::{load_state, require_accounts, require_signer, save_state};
use crate::transaction_context::TransactionContext;

pub(super) fn process_authorize(
    accounts: &[u8],
    new_auth: nusantara_crypto::Hash,
    auth_type: VoteAuthorize,
    ctx: &mut TransactionContext,
) -> Result<(), RuntimeError> {
    require_accounts(accounts, 2, "Authorize")?;
    let vote_idx = accounts[0] as usize;
    let auth_idx = accounts[1] as usize;

    let auth_address = require_signer(ctx, auth_idx)?;

    let mut state: VoteState = load_state(ctx, vote_idx)?;

    match auth_type {
        VoteAuthorize::Voter => {
            if state.authorized_voter != auth_address {
                return Err(RuntimeError::ProgramError {
                    program: "vote".to_string(),
                    message: "not authorized voter".to_string(),
                });
            }
            state.authorized_voter = new_auth;
        }
        VoteAuthorize::Withdrawer => {
            if state.authorized_withdrawer != auth_address {
                return Err(RuntimeError::ProgramError {
                    program: "vote".to_string(),
                    message: "not authorized withdrawer".to_string(),
                });
            }
            state.authorized_withdrawer = new_auth;
        }
    }

    save_state(ctx, vote_idx, &state)
}
