use borsh::BorshDeserialize;
use nusantara_core::program::STAKE_PROGRAM_ID;
use nusantara_core::{Account, EpochSchedule, FeeCalculator, Message, Transaction};
use nusantara_crypto::{Hash, Keypair, hash};
use nusantara_rent_program::Rent;
use nusantara_runtime::{ProgramCache, SysvarCache, execute_transaction};
use nusantara_stake_program::{Authorized, Lockup, StakeStateV2};
use nusantara_storage::Storage;
use nusantara_sysvar_program::{Clock, RecentBlockhashes, SlotHashes, StakeHistory};
use tempfile::tempdir;

fn test_sysvars(epoch: u64) -> SysvarCache {
    SysvarCache::new(
        Clock {
            slot: epoch * 432_000,
            epoch,
            unix_timestamp: 1_000_000,
            ..Clock::default()
        },
        Rent::default(),
        EpochSchedule::default(),
        SlotHashes::default(),
        StakeHistory::default(),
        RecentBlockhashes::new(vec![Hash::zero()]),
    )
}

fn test_storage() -> (Storage, tempfile::TempDir) {
    let dir = tempdir().unwrap();
    let storage = Storage::open(dir.path()).unwrap();
    (storage, dir)
}

fn commit_deltas(storage: &Storage, result: &nusantara_runtime::TransactionResult, slot: u64) {
    for (addr, account) in &result.account_deltas {
        storage.put_account(addr, slot, account).unwrap();
    }
}

#[test]
fn initialize_delegate_deactivate_withdraw() {
    let (storage, _dir) = test_storage();
    let staker_kp = Keypair::generate();
    let staker = staker_kp.address();
    let withdrawer_kp = Keypair::generate();
    let withdrawer = withdrawer_kp.address();
    let stake_acc_kp = Keypair::generate();
    let stake_acc = stake_acc_kp.address();
    let vote_acc = hash(b"vote_acc");
    let fee_calc = FeeCalculator::default();

    let rent = Rent::default();
    let authorized = Authorized { staker, withdrawer };
    let lockup = Lockup {
        unix_timestamp: 0,
        epoch: 0,
        custodian: Hash::zero(),
    };

    // Estimate state size
    let sample_state = StakeStateV2::Initialized(nusantara_stake_program::Meta {
        rent_exempt_reserve: 0,
        authorized: authorized.clone(),
        lockup: lockup.clone(),
    });
    let state_size = borsh::to_vec(&sample_state).unwrap().len();
    let min_rent = rent.minimum_balance(state_size);

    // Fund accounts
    storage
        .put_account(&staker, 0, &Account::new(10_000_000, Hash::zero()))
        .unwrap();
    storage
        .put_account(&withdrawer, 0, &Account::new(10_000_000, Hash::zero()))
        .unwrap();
    let mut stake_account = Account::new(min_rent + 2_000_000_000, *STAKE_PROGRAM_ID);
    stake_account.data = vec![0u8; state_size];
    storage.put_account(&stake_acc, 0, &stake_account).unwrap();
    storage
        .put_account(&vote_acc, 0, &Account::new(1_000_000, Hash::zero()))
        .unwrap();

    // Step 1: Initialize
    let init_ix =
        nusantara_stake_program::initialize(&stake_acc, authorized.clone(), lockup.clone());
    let init_msg = Message::new(&[init_ix], &stake_acc).unwrap();
    let mut init_tx = Transaction::new(init_msg);
    init_tx.sign(&[&stake_acc_kp]);
    let sysvars = test_sysvars(5);

    let cache = ProgramCache::new(16);
    let result = execute_transaction(&init_tx, &storage, &sysvars, &fee_calc, 100, &cache, None, false);
    assert!(result.status.is_ok(), "init failed: {:?}", result.status);
    commit_deltas(&storage, &result, 100);

    // Verify initialized
    let loaded = storage.get_account(&stake_acc).unwrap().unwrap();
    let state = StakeStateV2::try_from_slice(&loaded.data).unwrap();
    assert!(matches!(state, StakeStateV2::Initialized(_)));

    // Step 2: Delegate
    let del_ix = nusantara_stake_program::delegate_stake(&stake_acc, &vote_acc, &staker);
    let del_msg = Message::new(&[del_ix], &staker).unwrap();
    let mut del_tx = Transaction::new(del_msg);
    del_tx.sign(&[&staker_kp]);

    let result = execute_transaction(&del_tx, &storage, &sysvars, &fee_calc, 101, &cache, None, false);
    assert!(
        result.status.is_ok(),
        "delegate failed: {:?}",
        result.status
    );
    commit_deltas(&storage, &result, 101);

    // Verify delegated
    let loaded = storage.get_account(&stake_acc).unwrap().unwrap();
    let state = StakeStateV2::try_from_slice(&loaded.data).unwrap();
    assert!(matches!(state, StakeStateV2::Stake(_, _)));

    // Step 3: Deactivate
    let deact_ix = nusantara_stake_program::deactivate(&stake_acc, &staker);
    let deact_msg = Message::new(&[deact_ix], &staker).unwrap();
    let mut deact_tx = Transaction::new(deact_msg);
    deact_tx.sign(&[&staker_kp]);

    let result = execute_transaction(&deact_tx, &storage, &sysvars, &fee_calc, 102, &cache, None, false);
    assert!(
        result.status.is_ok(),
        "deactivate failed: {:?}",
        result.status
    );
    commit_deltas(&storage, &result, 102);

    // Verify deactivation epoch set
    let loaded = storage.get_account(&stake_acc).unwrap().unwrap();
    let state = StakeStateV2::try_from_slice(&loaded.data).unwrap();
    if let StakeStateV2::Stake(_, s) = &state {
        assert_eq!(s.delegation.deactivation_epoch, 5);
    } else {
        panic!("expected Stake state");
    }

    // Step 4: Withdraw (after deactivation epoch)
    let later_sysvars = test_sysvars(10); // epoch 10 > deactivation epoch 5
    let dest = hash(b"destination");
    let w_ix = nusantara_stake_program::withdraw(&stake_acc, &withdrawer, &dest, 500_000);
    let w_msg = Message::new(&[w_ix], &withdrawer).unwrap();
    let mut w_tx = Transaction::new(w_msg);
    w_tx.sign(&[&withdrawer_kp]);

    let result = execute_transaction(
        &w_tx,
        &storage,
        &later_sysvars,
        &fee_calc,
        200,
        &cache,
        None,
        false,
    );
    assert!(
        result.status.is_ok(),
        "withdraw failed: {:?}",
        result.status
    );
    commit_deltas(&storage, &result, 200);

    let dest_acc = storage.get_account(&dest).unwrap().unwrap();
    assert_eq!(dest_acc.lamports, 500_000);
}

#[test]
fn split_stake() {
    let (storage, _dir) = test_storage();
    let staker_kp = Keypair::generate();
    let staker = staker_kp.address();
    let withdrawer = hash(b"withdrawer");
    let stake_acc_kp = Keypair::generate();
    let stake_acc = stake_acc_kp.address();
    let split_acc = hash(b"split_acc");
    let vote_acc = hash(b"vote_acc");
    let fee_calc = FeeCalculator::default();

    let rent = Rent::default();
    let authorized = Authorized { staker, withdrawer };
    let lockup = Lockup {
        unix_timestamp: 0,
        epoch: 0,
        custodian: Hash::zero(),
    };

    let sample_state = StakeStateV2::Initialized(nusantara_stake_program::Meta {
        rent_exempt_reserve: 0,
        authorized: authorized.clone(),
        lockup: lockup.clone(),
    });
    let state_size = borsh::to_vec(&sample_state).unwrap().len();
    let min_rent = rent.minimum_balance(state_size);

    // Fund
    storage
        .put_account(&staker, 0, &Account::new(10_000_000, Hash::zero()))
        .unwrap();
    let mut sa = Account::new(min_rent + 2_000_000_000, *STAKE_PROGRAM_ID);
    sa.data = vec![0u8; state_size];
    storage.put_account(&stake_acc, 0, &sa).unwrap();
    let mut sp = Account::new(0, *STAKE_PROGRAM_ID);
    sp.data = vec![0u8; state_size];
    storage.put_account(&split_acc, 0, &sp).unwrap();
    storage
        .put_account(&vote_acc, 0, &Account::new(1_000_000, Hash::zero()))
        .unwrap();

    let sysvars = test_sysvars(5);

    // Initialize
    let init_ix = nusantara_stake_program::initialize(&stake_acc, authorized, lockup);
    let init_msg = Message::new(&[init_ix], &stake_acc).unwrap();
    let mut init_tx = Transaction::new(init_msg);
    init_tx.sign(&[&stake_acc_kp]);
    let cache = ProgramCache::new(16);
    let result = execute_transaction(&init_tx, &storage, &sysvars, &fee_calc, 100, &cache, None, false);
    assert!(result.status.is_ok());
    commit_deltas(&storage, &result, 100);

    // Split
    let split_ix = nusantara_stake_program::split(&stake_acc, &staker, &split_acc, 500_000_000);
    let split_msg = Message::new(&[split_ix], &staker).unwrap();
    let mut split_tx = Transaction::new(split_msg);
    split_tx.sign(&[&staker_kp]);
    let result = execute_transaction(&split_tx, &storage, &sysvars, &fee_calc, 101, &cache, None, false);
    assert!(result.status.is_ok(), "split failed: {:?}", result.status);
    commit_deltas(&storage, &result, 101);

    let loaded_split = storage.get_account(&split_acc).unwrap().unwrap();
    assert!(loaded_split.lamports >= 500_000_000);
    let state = StakeStateV2::try_from_slice(&loaded_split.data).unwrap();
    assert!(matches!(state, StakeStateV2::Initialized(_)));
}
