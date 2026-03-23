use borsh::BorshDeserialize;
use nusantara_vote_program::VoteInstruction;

use crate::cost_schedule::VOTE_BASE_COST;
use crate::error::RuntimeError;
use crate::sysvar_cache::SysvarCache;
use crate::transaction_context::TransactionContext;

mod authorize;
mod commission;
mod initialize;
mod vote_action;
mod withdraw;

#[tracing::instrument(skip_all, fields(program = "vote"))]
pub fn process_vote(
    accounts: &[u8],
    data: &[u8],
    ctx: &mut TransactionContext,
    sysvars: &SysvarCache,
) -> Result<(), RuntimeError> {
    ctx.consume_compute(VOTE_BASE_COST)?;

    let instruction = VoteInstruction::try_from_slice(data)
        .map_err(|e| RuntimeError::InvalidInstructionData(e.to_string()))?;

    match instruction {
        VoteInstruction::InitializeAccount(init) => {
            initialize::process_initialize(accounts, init, ctx, sysvars)
        }
        VoteInstruction::Vote(vote) => {
            vote_action::process_vote_action(accounts, vote, ctx, sysvars)
        }
        VoteInstruction::Authorize(new_auth, auth_type) => {
            authorize::process_authorize(accounts, new_auth, auth_type, ctx)
        }
        VoteInstruction::Withdraw(lamports) => {
            withdraw::process_withdraw(accounts, lamports, ctx, sysvars)
        }
        VoteInstruction::UpdateCommission(commission) => {
            commission::process_update_commission(accounts, commission, ctx)
        }
        VoteInstruction::SwitchVote(vote, _proof_hash) => {
            vote_action::process_vote_action(accounts, vote, ctx, sysvars)
        }
        VoteInstruction::UpdateValidatorIdentity => Err(RuntimeError::ProgramError {
            program: "vote".to_string(),
            message: "instruction not yet implemented".to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nusantara_core::program::VOTE_PROGRAM_ID;
    use nusantara_core::{Account, Message};
    use nusantara_crypto::hash;
    use nusantara_vote_program::{Vote, VoteAuthorize, VoteInit, VoteState};

    use crate::test_utils::test_sysvars_with_clock;

    fn test_sysvars() -> SysvarCache {
        test_sysvars_with_clock(100, 5, 1_000_000)
    }

    fn setup_vote_init() -> (TransactionContext, Vec<u8>, Vec<u8>, SysvarCache) {
        let vote_acc = hash(b"vote");
        let node = hash(b"node");
        let voter = hash(b"voter");
        let withdrawer = hash(b"withdrawer");

        let init = VoteInit {
            node_pubkey: node,
            authorized_voter: voter,
            authorized_withdrawer: withdrawer,
            commission: 10,
        };

        let ix = nusantara_vote_program::initialize_account(&vote_acc, init);
        let msg = Message::new(&[ix], &vote_acc).unwrap();

        let accounts: Vec<_> = msg
            .account_keys
            .iter()
            .map(|k| {
                if k == &vote_acc {
                    (*k, Account::new(10_000_000, *VOTE_PROGRAM_ID))
                } else {
                    (*k, Account::new(0, nusantara_crypto::Hash::zero()))
                }
            })
            .collect();

        let compiled = msg.instructions[0].accounts.clone();
        let data = msg.instructions[0].data.clone();
        let ctx = TransactionContext::new(accounts, msg, 100, 100_000);
        (ctx, compiled, data, test_sysvars())
    }

    #[test]
    fn initialize_success() {
        let (mut ctx, accounts, data, sysvars) = setup_vote_init();
        process_vote(&accounts, &data, &mut ctx, &sysvars).unwrap();

        let idx = ctx
            .message()
            .account_keys
            .iter()
            .position(|k| k == &hash(b"vote"))
            .unwrap();
        let acc = ctx.get_account(idx).unwrap();
        let state = VoteState::try_from_slice(&acc.account.data).unwrap();
        assert_eq!(state.commission, 10);
        assert_eq!(state.authorized_voter, hash(b"voter"));
    }

    #[test]
    fn initialize_already_initialized() {
        let (mut ctx, accounts, data, sysvars) = setup_vote_init();
        process_vote(&accounts, &data, &mut ctx, &sysvars).unwrap();
        // Re-init
        let mut ctx2 = TransactionContext::new(
            ctx.message()
                .account_keys
                .iter()
                .enumerate()
                .map(|(i, k)| (*k, ctx.get_account(i).unwrap().account.clone()))
                .collect(),
            ctx.message().clone(),
            100,
            100_000,
        );
        let err = process_vote(&accounts, &data, &mut ctx2, &sysvars).unwrap_err();
        assert!(matches!(err, RuntimeError::InvalidAccountData(_)));
    }

    #[test]
    fn vote_success() {
        let (mut ctx, accounts, data, sysvars) = setup_vote_init();
        process_vote(&accounts, &data, &mut ctx, &sysvars).unwrap();

        let vote_acc = hash(b"vote");
        let voter = hash(b"voter");
        let v = Vote {
            slots: vec![97, 98, 99],
            hash: hash(b"blockhash"),
            timestamp: Some(1_000_000),
        };
        let vote_ix = nusantara_vote_program::vote(&vote_acc, &voter, v);
        let vote_msg = Message::new(&[vote_ix], &voter).unwrap();

        let vote_accounts: Vec<_> = vote_msg
            .account_keys
            .iter()
            .map(|k| {
                if k == &vote_acc {
                    let idx = ctx
                        .message()
                        .account_keys
                        .iter()
                        .position(|a| a == &vote_acc)
                        .unwrap();
                    (*k, ctx.get_account(idx).unwrap().account.clone())
                } else {
                    (*k, Account::new(1_000_000, nusantara_crypto::Hash::zero()))
                }
            })
            .collect();

        let compiled = vote_msg.instructions[0].accounts.clone();
        let vote_data = vote_msg.instructions[0].data.clone();
        let mut vote_ctx = TransactionContext::new(vote_accounts, vote_msg, 100, 100_000);
        process_vote(&compiled, &vote_data, &mut vote_ctx, &sysvars).unwrap();

        let idx = vote_ctx
            .message()
            .account_keys
            .iter()
            .position(|k| k == &vote_acc)
            .unwrap();
        let acc = vote_ctx.get_account(idx).unwrap();
        let state = VoteState::try_from_slice(&acc.account.data).unwrap();
        assert_eq!(state.votes.len(), 3);
    }

    #[test]
    fn vote_not_authorized() {
        let (mut ctx, accounts, data, sysvars) = setup_vote_init();
        process_vote(&accounts, &data, &mut ctx, &sysvars).unwrap();

        let vote_acc = hash(b"vote");
        let wrong_voter = hash(b"wrong");
        let v = Vote {
            slots: vec![100],
            hash: hash(b"blockhash"),
            timestamp: None,
        };
        let vote_ix = nusantara_vote_program::vote(&vote_acc, &wrong_voter, v);
        let vote_msg = Message::new(&[vote_ix], &wrong_voter).unwrap();

        let vote_accounts: Vec<_> = vote_msg
            .account_keys
            .iter()
            .map(|k| {
                if k == &vote_acc {
                    let idx = ctx
                        .message()
                        .account_keys
                        .iter()
                        .position(|a| a == &vote_acc)
                        .unwrap();
                    (*k, ctx.get_account(idx).unwrap().account.clone())
                } else {
                    (*k, Account::new(1_000_000, nusantara_crypto::Hash::zero()))
                }
            })
            .collect();

        let compiled = vote_msg.instructions[0].accounts.clone();
        let vote_data = vote_msg.instructions[0].data.clone();
        let mut vote_ctx = TransactionContext::new(vote_accounts, vote_msg, 100, 100_000);
        let err = process_vote(&compiled, &vote_data, &mut vote_ctx, &sysvars).unwrap_err();
        assert!(matches!(err, RuntimeError::ProgramError { .. }));
    }

    #[test]
    fn authorize_voter() {
        let (mut ctx, accounts, data, sysvars) = setup_vote_init();
        process_vote(&accounts, &data, &mut ctx, &sysvars).unwrap();

        let vote_acc = hash(b"vote");
        let voter = hash(b"voter");
        let new_voter = hash(b"new_voter");
        let auth_ix =
            nusantara_vote_program::authorize(&vote_acc, &voter, new_voter, VoteAuthorize::Voter);
        let auth_msg = Message::new(&[auth_ix], &voter).unwrap();

        let auth_accounts: Vec<_> = auth_msg
            .account_keys
            .iter()
            .map(|k| {
                if k == &vote_acc {
                    let idx = ctx
                        .message()
                        .account_keys
                        .iter()
                        .position(|a| a == &vote_acc)
                        .unwrap();
                    (*k, ctx.get_account(idx).unwrap().account.clone())
                } else {
                    (*k, Account::new(1_000_000, nusantara_crypto::Hash::zero()))
                }
            })
            .collect();

        let compiled = auth_msg.instructions[0].accounts.clone();
        let auth_data = auth_msg.instructions[0].data.clone();
        let mut auth_ctx = TransactionContext::new(auth_accounts, auth_msg, 100, 100_000);
        process_vote(&compiled, &auth_data, &mut auth_ctx, &sysvars).unwrap();

        let idx = auth_ctx
            .message()
            .account_keys
            .iter()
            .position(|k| k == &vote_acc)
            .unwrap();
        let acc = auth_ctx.get_account(idx).unwrap();
        let state = VoteState::try_from_slice(&acc.account.data).unwrap();
        assert_eq!(state.authorized_voter, new_voter);
    }

    #[test]
    fn authorize_withdrawer() {
        let (mut ctx, accounts, data, sysvars) = setup_vote_init();
        process_vote(&accounts, &data, &mut ctx, &sysvars).unwrap();

        let vote_acc = hash(b"vote");
        let withdrawer = hash(b"withdrawer");
        let new_withdrawer = hash(b"new_withdrawer");
        let auth_ix = nusantara_vote_program::authorize(
            &vote_acc,
            &withdrawer,
            new_withdrawer,
            VoteAuthorize::Withdrawer,
        );
        let auth_msg = Message::new(&[auth_ix], &withdrawer).unwrap();

        let auth_accounts: Vec<_> = auth_msg
            .account_keys
            .iter()
            .map(|k| {
                if k == &vote_acc {
                    let idx = ctx
                        .message()
                        .account_keys
                        .iter()
                        .position(|a| a == &vote_acc)
                        .unwrap();
                    (*k, ctx.get_account(idx).unwrap().account.clone())
                } else {
                    (*k, Account::new(1_000_000, nusantara_crypto::Hash::zero()))
                }
            })
            .collect();

        let compiled = auth_msg.instructions[0].accounts.clone();
        let auth_data = auth_msg.instructions[0].data.clone();
        let mut auth_ctx = TransactionContext::new(auth_accounts, auth_msg, 100, 100_000);
        process_vote(&compiled, &auth_data, &mut auth_ctx, &sysvars).unwrap();

        let idx = auth_ctx
            .message()
            .account_keys
            .iter()
            .position(|k| k == &vote_acc)
            .unwrap();
        let acc = auth_ctx.get_account(idx).unwrap();
        let state = VoteState::try_from_slice(&acc.account.data).unwrap();
        assert_eq!(state.authorized_withdrawer, new_withdrawer);
    }

    #[test]
    fn withdraw_success() {
        let (mut ctx, accounts, data, sysvars) = setup_vote_init();
        process_vote(&accounts, &data, &mut ctx, &sysvars).unwrap();

        let vote_acc = hash(b"vote");
        let withdrawer = hash(b"withdrawer");
        let to = hash(b"to");
        let w_ix = nusantara_vote_program::withdraw(&vote_acc, &withdrawer, &to, 100_000);
        let w_msg = Message::new(&[w_ix], &withdrawer).unwrap();

        let w_accounts: Vec<_> = w_msg
            .account_keys
            .iter()
            .map(|k| {
                if k == &vote_acc {
                    let idx = ctx
                        .message()
                        .account_keys
                        .iter()
                        .position(|a| a == &vote_acc)
                        .unwrap();
                    (*k, ctx.get_account(idx).unwrap().account.clone())
                } else if k == &withdrawer {
                    (*k, Account::new(1_000_000, nusantara_crypto::Hash::zero()))
                } else {
                    (*k, Account::new(0, nusantara_crypto::Hash::zero()))
                }
            })
            .collect();

        let compiled = w_msg.instructions[0].accounts.clone();
        let w_data = w_msg.instructions[0].data.clone();
        let mut w_ctx = TransactionContext::new(w_accounts, w_msg, 100, 100_000);
        process_vote(&compiled, &w_data, &mut w_ctx, &sysvars).unwrap();
    }

    #[test]
    fn update_commission() {
        let (mut ctx, accounts, data, sysvars) = setup_vote_init();
        process_vote(&accounts, &data, &mut ctx, &sysvars).unwrap();

        let vote_acc = hash(b"vote");
        let withdrawer = hash(b"withdrawer");
        let ix = nusantara_vote_program::update_commission(&vote_acc, &withdrawer, 25);
        let msg = Message::new(&[ix], &withdrawer).unwrap();

        let c_accounts: Vec<_> = msg
            .account_keys
            .iter()
            .map(|k| {
                if k == &vote_acc {
                    let idx = ctx
                        .message()
                        .account_keys
                        .iter()
                        .position(|a| a == &vote_acc)
                        .unwrap();
                    (*k, ctx.get_account(idx).unwrap().account.clone())
                } else {
                    (*k, Account::new(1_000_000, nusantara_crypto::Hash::zero()))
                }
            })
            .collect();

        let compiled = msg.instructions[0].accounts.clone();
        let c_data = msg.instructions[0].data.clone();
        let mut c_ctx = TransactionContext::new(c_accounts, msg, 100, 100_000);
        process_vote(&compiled, &c_data, &mut c_ctx, &sysvars).unwrap();

        let idx = c_ctx
            .message()
            .account_keys
            .iter()
            .position(|k| k == &vote_acc)
            .unwrap();
        let acc = c_ctx.get_account(idx).unwrap();
        let state = VoteState::try_from_slice(&acc.account.data).unwrap();
        assert_eq!(state.commission, 25);
    }

    #[test]
    fn update_commission_unauthorized() {
        let (mut ctx, accounts, data, sysvars) = setup_vote_init();
        process_vote(&accounts, &data, &mut ctx, &sysvars).unwrap();

        let vote_acc = hash(b"vote");
        let wrong_auth = hash(b"wrong");
        let ix = nusantara_vote_program::update_commission(&vote_acc, &wrong_auth, 25);
        let msg = Message::new(&[ix], &wrong_auth).unwrap();

        let c_accounts: Vec<_> = msg
            .account_keys
            .iter()
            .map(|k| {
                if k == &vote_acc {
                    let idx = ctx
                        .message()
                        .account_keys
                        .iter()
                        .position(|a| a == &vote_acc)
                        .unwrap();
                    (*k, ctx.get_account(idx).unwrap().account.clone())
                } else {
                    (*k, Account::new(1_000_000, nusantara_crypto::Hash::zero()))
                }
            })
            .collect();

        let compiled = msg.instructions[0].accounts.clone();
        let c_data = msg.instructions[0].data.clone();
        let mut c_ctx = TransactionContext::new(c_accounts, msg, 100, 100_000);
        let err = process_vote(&compiled, &c_data, &mut c_ctx, &sysvars).unwrap_err();
        assert!(matches!(err, RuntimeError::ProgramError { .. }));
    }

    #[test]
    fn update_commission_over_100() {
        let (mut ctx, accounts, data, sysvars) = setup_vote_init();
        process_vote(&accounts, &data, &mut ctx, &sysvars).unwrap();

        let vote_acc = hash(b"vote");
        let withdrawer = hash(b"withdrawer");
        let ix = nusantara_vote_program::update_commission(&vote_acc, &withdrawer, 150);
        let msg = Message::new(&[ix], &withdrawer).unwrap();

        let c_accounts: Vec<_> = msg
            .account_keys
            .iter()
            .map(|k| {
                if k == &vote_acc {
                    let idx = ctx
                        .message()
                        .account_keys
                        .iter()
                        .position(|a| a == &vote_acc)
                        .unwrap();
                    (*k, ctx.get_account(idx).unwrap().account.clone())
                } else {
                    (*k, Account::new(1_000_000, nusantara_crypto::Hash::zero()))
                }
            })
            .collect();

        let compiled = msg.instructions[0].accounts.clone();
        let c_data = msg.instructions[0].data.clone();
        let mut c_ctx = TransactionContext::new(c_accounts, msg, 100, 100_000);
        let err = process_vote(&compiled, &c_data, &mut c_ctx, &sysvars).unwrap_err();
        assert!(matches!(err, RuntimeError::ProgramError { .. }));
    }
}
