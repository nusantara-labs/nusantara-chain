mod deactivate;
mod delegate;
mod initialize;
mod split;
mod withdraw;

use borsh::BorshDeserialize;
use nusantara_stake_program::StakeInstruction;

use crate::cost_schedule::STAKE_BASE_COST;
use crate::error::RuntimeError;
use crate::sysvar_cache::SysvarCache;
use crate::transaction_context::TransactionContext;

#[tracing::instrument(skip_all, fields(program = "stake"))]
pub fn process_stake(
    accounts: &[u8],
    data: &[u8],
    ctx: &mut TransactionContext,
    sysvars: &SysvarCache,
) -> Result<(), RuntimeError> {
    ctx.consume_compute(STAKE_BASE_COST)?;

    let instruction = StakeInstruction::try_from_slice(data)
        .map_err(|e| RuntimeError::InvalidInstructionData(e.to_string()))?;

    match instruction {
        StakeInstruction::Initialize(authorized, lockup) => {
            initialize::process_initialize(accounts, authorized, lockup, ctx, sysvars)
        }
        StakeInstruction::DelegateStake => delegate::process_delegate(accounts, ctx, sysvars),
        StakeInstruction::Deactivate => deactivate::process_deactivate(accounts, ctx, sysvars),
        StakeInstruction::Withdraw(lamports) => {
            withdraw::process_withdraw(accounts, lamports, ctx, sysvars)
        }
        StakeInstruction::Split(lamports) => split::process_split(accounts, lamports, ctx, sysvars),
        StakeInstruction::Merge
        | StakeInstruction::Authorize(_, _)
        | StakeInstruction::SetLockup(_) => Err(RuntimeError::ProgramError {
            program: "stake".to_string(),
            message: "instruction not yet implemented".to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nusantara_core::program::STAKE_PROGRAM_ID;
    use nusantara_core::{Account, Message};
    use nusantara_crypto::{Hash, hash};
    use nusantara_rent_program::Rent;
    use nusantara_stake_program::{Authorized, Lockup, Meta, StakeStateV2};

    use crate::sysvar_cache::SysvarCache;
    use crate::test_utils::test_sysvars_with_clock;
    use crate::transaction_context::TransactionContext;

    fn test_sysvars() -> SysvarCache {
        test_sysvars_with_clock(100, 5, 1_000_000)
    }

    fn setup_stake_init() -> (TransactionContext, Vec<u8>, Vec<u8>, SysvarCache) {
        let stake_acc = hash(b"stake");
        let staker = hash(b"staker");
        let withdrawer = hash(b"withdrawer");

        let authorized = Authorized { staker, withdrawer };
        let lockup = Lockup {
            unix_timestamp: 0,
            epoch: 0,
            custodian: Hash::zero(),
        };

        let ix = nusantara_stake_program::initialize(&stake_acc, authorized, lockup);
        let msg = Message::new(&[ix], &stake_acc).unwrap();

        let rent = Rent::default();
        // Estimate data size for StakeStateV2::Initialized
        let state = StakeStateV2::Initialized(Meta {
            rent_exempt_reserve: 0,
            authorized: Authorized { staker, withdrawer },
            lockup: Lockup {
                unix_timestamp: 0,
                epoch: 0,
                custodian: Hash::zero(),
            },
        });
        let state_size = borsh::to_vec(&state).unwrap().len();
        let min = rent.minimum_balance(state_size);

        let accounts: Vec<_> = msg
            .account_keys
            .iter()
            .map(|k| {
                if k == &stake_acc {
                    let mut a = Account::new(min + 1_000_000_000, *STAKE_PROGRAM_ID);
                    a.data = vec![0u8; state_size]; // pre-allocate
                    (*k, a)
                } else {
                    (*k, Account::new(0, Hash::zero()))
                }
            })
            .collect();

        let compiled_accounts = msg.instructions[0].accounts.clone();
        let data = msg.instructions[0].data.clone();
        let ctx = TransactionContext::new(accounts, msg, 100, 100_000);
        (ctx, compiled_accounts, data, test_sysvars())
    }

    #[test]
    fn initialize_success() {
        let (mut ctx, accounts, data, sysvars) = setup_stake_init();
        process_stake(&accounts, &data, &mut ctx, &sysvars).unwrap();

        let stake_idx = ctx
            .message()
            .account_keys
            .iter()
            .position(|k| k == &hash(b"stake"))
            .unwrap();
        let acc = ctx.get_account(stake_idx).unwrap();
        let state = StakeStateV2::try_from_slice(&acc.account.data).unwrap();
        assert!(matches!(state, StakeStateV2::Initialized(_)));
    }

    #[test]
    fn initialize_already_initialized() {
        let (mut ctx, accounts, data, sysvars) = setup_stake_init();
        process_stake(&accounts, &data, &mut ctx, &sysvars).unwrap();
        // Try to initialize again
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
        let err = process_stake(&accounts, &data, &mut ctx2, &sysvars).unwrap_err();
        assert!(matches!(err, RuntimeError::InvalidAccountData(_)));
    }

    #[test]
    fn delegate_success() {
        let (mut ctx, accounts, data, sysvars) = setup_stake_init();
        process_stake(&accounts, &data, &mut ctx, &sysvars).unwrap();

        // Now delegate
        let stake_acc = hash(b"stake");
        let vote_acc = hash(b"vote");
        let staker = hash(b"staker");
        let del_ix = nusantara_stake_program::delegate_stake(&stake_acc, &vote_acc, &staker);
        let del_msg = Message::new(&[del_ix], &staker).unwrap();

        let del_accounts: Vec<_> = del_msg
            .account_keys
            .iter()
            .map(|k| {
                if k == &stake_acc {
                    let stake_idx = ctx
                        .message()
                        .account_keys
                        .iter()
                        .position(|a| a == &stake_acc)
                        .unwrap();
                    (*k, ctx.get_account(stake_idx).unwrap().account.clone())
                } else if k == &staker {
                    (*k, Account::new(1_000_000, Hash::zero()))
                } else {
                    (*k, Account::new(0, Hash::zero()))
                }
            })
            .collect();

        let compiled = del_msg.instructions[0].accounts.clone();
        let del_data = del_msg.instructions[0].data.clone();
        let mut del_ctx = TransactionContext::new(del_accounts, del_msg, 100, 100_000);
        process_stake(&compiled, &del_data, &mut del_ctx, &sysvars).unwrap();

        let idx = del_ctx
            .message()
            .account_keys
            .iter()
            .position(|k| k == &stake_acc)
            .unwrap();
        let acc = del_ctx.get_account(idx).unwrap();
        let state = StakeStateV2::try_from_slice(&acc.account.data).unwrap();
        assert!(matches!(state, StakeStateV2::Stake(_, _)));
    }

    #[test]
    fn delegate_not_initialized() {
        let stake_acc = hash(b"stake");
        let vote_acc = hash(b"vote");
        let staker = hash(b"staker");
        let ix = nusantara_stake_program::delegate_stake(&stake_acc, &vote_acc, &staker);
        let msg = Message::new(&[ix], &staker).unwrap();
        let accounts: Vec<_> = msg
            .account_keys
            .iter()
            .map(|k| {
                if k == &stake_acc {
                    let mut a = Account::new(2_000_000_000, *STAKE_PROGRAM_ID);
                    let state = StakeStateV2::Uninitialized;
                    a.data = borsh::to_vec(&state).unwrap();
                    (*k, a)
                } else {
                    (*k, Account::new(1_000_000, Hash::zero()))
                }
            })
            .collect();
        let compiled = msg.instructions[0].accounts.clone();
        let data = msg.instructions[0].data.clone();
        let mut ctx = TransactionContext::new(accounts, msg, 100, 100_000);
        let err = process_stake(&compiled, &data, &mut ctx, &test_sysvars()).unwrap_err();
        assert!(matches!(err, RuntimeError::InvalidAccountData(_)));
    }

    #[test]
    fn delegate_wrong_signer() {
        let (mut ctx, accounts, data, sysvars) = setup_stake_init();
        process_stake(&accounts, &data, &mut ctx, &sysvars).unwrap();

        let stake_acc = hash(b"stake");
        let vote_acc = hash(b"vote");
        let wrong_staker = hash(b"wrong_staker");
        let del_ix = nusantara_stake_program::delegate_stake(&stake_acc, &vote_acc, &wrong_staker);
        let del_msg = Message::new(&[del_ix], &wrong_staker).unwrap();

        let del_accounts: Vec<_> = del_msg
            .account_keys
            .iter()
            .map(|k| {
                if k == &stake_acc {
                    let idx = ctx
                        .message()
                        .account_keys
                        .iter()
                        .position(|a| a == &stake_acc)
                        .unwrap();
                    (*k, ctx.get_account(idx).unwrap().account.clone())
                } else {
                    (*k, Account::new(1_000_000, Hash::zero()))
                }
            })
            .collect();

        let compiled = del_msg.instructions[0].accounts.clone();
        let del_data = del_msg.instructions[0].data.clone();
        let mut del_ctx = TransactionContext::new(del_accounts, del_msg, 100, 100_000);
        let err = process_stake(&compiled, &del_data, &mut del_ctx, &sysvars).unwrap_err();
        assert!(matches!(err, RuntimeError::AccountNotSigner(_)));
    }

    #[test]
    fn withdraw_success() {
        let (mut ctx, accounts, data, sysvars) = setup_stake_init();
        process_stake(&accounts, &data, &mut ctx, &sysvars).unwrap();

        let stake_acc = hash(b"stake");
        let withdrawer = hash(b"withdrawer");
        let to = hash(b"to");
        let w_ix = nusantara_stake_program::withdraw(&stake_acc, &withdrawer, &to, 100_000);
        let w_msg = Message::new(&[w_ix], &withdrawer).unwrap();

        let w_accounts: Vec<_> = w_msg
            .account_keys
            .iter()
            .map(|k| {
                if k == &stake_acc {
                    let idx = ctx
                        .message()
                        .account_keys
                        .iter()
                        .position(|a| a == &stake_acc)
                        .unwrap();
                    (*k, ctx.get_account(idx).unwrap().account.clone())
                } else if k == &withdrawer {
                    (*k, Account::new(1_000_000, Hash::zero()))
                } else {
                    (*k, Account::new(0, Hash::zero()))
                }
            })
            .collect();

        let compiled = w_msg.instructions[0].accounts.clone();
        let w_data = w_msg.instructions[0].data.clone();
        let mut w_ctx = TransactionContext::new(w_accounts, w_msg, 100, 100_000);
        process_stake(&compiled, &w_data, &mut w_ctx, &sysvars).unwrap();
    }

    #[test]
    fn deactivate_success() {
        // First initialize and delegate, then deactivate
        let (mut ctx, accounts, data, sysvars) = setup_stake_init();
        process_stake(&accounts, &data, &mut ctx, &sysvars).unwrap();

        let stake_acc = hash(b"stake");
        let vote_acc = hash(b"vote");
        let staker = hash(b"staker");

        // Delegate first
        let del_ix = nusantara_stake_program::delegate_stake(&stake_acc, &vote_acc, &staker);
        let del_msg = Message::new(&[del_ix], &staker).unwrap();
        let del_accounts: Vec<_> = del_msg
            .account_keys
            .iter()
            .map(|k| {
                if k == &stake_acc {
                    let idx = ctx
                        .message()
                        .account_keys
                        .iter()
                        .position(|a| a == &stake_acc)
                        .unwrap();
                    (*k, ctx.get_account(idx).unwrap().account.clone())
                } else {
                    (*k, Account::new(1_000_000, Hash::zero()))
                }
            })
            .collect();
        let compiled_del = del_msg.instructions[0].accounts.clone();
        let del_data = del_msg.instructions[0].data.clone();
        let mut del_ctx = TransactionContext::new(del_accounts, del_msg, 100, 100_000);
        process_stake(&compiled_del, &del_data, &mut del_ctx, &sysvars).unwrap();

        // Now deactivate
        let deact_ix = nusantara_stake_program::deactivate(&stake_acc, &staker);
        let deact_msg = Message::new(&[deact_ix], &staker).unwrap();
        let deact_accounts: Vec<_> = deact_msg
            .account_keys
            .iter()
            .map(|k| {
                if k == &stake_acc {
                    let idx = del_ctx
                        .message()
                        .account_keys
                        .iter()
                        .position(|a| a == &stake_acc)
                        .unwrap();
                    (*k, del_ctx.get_account(idx).unwrap().account.clone())
                } else {
                    (*k, Account::new(1_000_000, Hash::zero()))
                }
            })
            .collect();
        let compiled_deact = deact_msg.instructions[0].accounts.clone();
        let deact_data = deact_msg.instructions[0].data.clone();
        let mut deact_ctx = TransactionContext::new(deact_accounts, deact_msg, 100, 100_000);
        process_stake(&compiled_deact, &deact_data, &mut deact_ctx, &sysvars).unwrap();

        let idx = deact_ctx
            .message()
            .account_keys
            .iter()
            .position(|k| k == &stake_acc)
            .unwrap();
        let acc = deact_ctx.get_account(idx).unwrap();
        let state = StakeStateV2::try_from_slice(&acc.account.data).unwrap();
        if let StakeStateV2::Stake(_, s) = state {
            assert_eq!(s.delegation.deactivation_epoch, 5);
        } else {
            panic!("expected Stake state");
        }
    }

    #[test]
    fn split_success() {
        let (mut ctx, accounts, data, sysvars) = setup_stake_init();
        process_stake(&accounts, &data, &mut ctx, &sysvars).unwrap();

        let stake_acc = hash(b"stake");
        let split_acc = hash(b"split");
        let staker = hash(b"staker");

        let split_ix = nusantara_stake_program::split(&stake_acc, &staker, &split_acc, 500_000_000);
        let split_msg = Message::new(&[split_ix], &staker).unwrap();

        let state_size = {
            let idx = ctx
                .message()
                .account_keys
                .iter()
                .position(|a| a == &stake_acc)
                .unwrap();
            ctx.get_account(idx).unwrap().account.data.len()
        };

        let split_accounts: Vec<_> = split_msg
            .account_keys
            .iter()
            .map(|k| {
                if k == &stake_acc {
                    let idx = ctx
                        .message()
                        .account_keys
                        .iter()
                        .position(|a| a == &stake_acc)
                        .unwrap();
                    (*k, ctx.get_account(idx).unwrap().account.clone())
                } else if k == &split_acc {
                    let mut a = Account::new(0, *STAKE_PROGRAM_ID);
                    a.data = vec![0u8; state_size];
                    (*k, a)
                } else {
                    (*k, Account::new(1_000_000, Hash::zero()))
                }
            })
            .collect();

        let compiled = split_msg.instructions[0].accounts.clone();
        let split_data = split_msg.instructions[0].data.clone();
        let mut split_ctx = TransactionContext::new(split_accounts, split_msg, 100, 100_000);
        process_stake(&compiled, &split_data, &mut split_ctx, &sysvars).unwrap();
    }
}
