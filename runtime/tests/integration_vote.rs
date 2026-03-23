use borsh::BorshDeserialize;
use nusantara_core::program::VOTE_PROGRAM_ID;
use nusantara_core::{Account, EpochSchedule, FeeCalculator, Message, Transaction};
use nusantara_crypto::{Hash, Keypair, hash};
use nusantara_rent_program::Rent;
use nusantara_runtime::{ProgramCache, SysvarCache, execute_transaction};
use nusantara_storage::Storage;
use nusantara_sysvar_program::{Clock, RecentBlockhashes, SlotHashes, StakeHistory};
use nusantara_vote_program::{Vote, VoteAuthorize, VoteInit, VoteState};
use tempfile::tempdir;

fn test_sysvars() -> SysvarCache {
    SysvarCache::new(
        Clock {
            slot: 100,
            epoch: 5,
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
fn initialize_vote_cast_authorize_withdraw() {
    let (storage, _dir) = test_storage();
    let vote_acc_kp = Keypair::generate();
    let vote_acc = vote_acc_kp.address();
    let node = hash(b"node");
    let voter_kp = Keypair::generate();
    let voter = voter_kp.address();
    let withdrawer_kp = Keypair::generate();
    let withdrawer = withdrawer_kp.address();
    let fee_calc = FeeCalculator::default();
    let sysvars = test_sysvars();

    // Fund accounts
    storage
        .put_account(&vote_acc, 0, &Account::new(10_000_000, *VOTE_PROGRAM_ID))
        .unwrap();
    storage
        .put_account(&voter, 0, &Account::new(1_000_000, Hash::zero()))
        .unwrap();
    storage
        .put_account(&withdrawer, 0, &Account::new(1_000_000, Hash::zero()))
        .unwrap();

    // Step 1: Initialize
    let init = VoteInit {
        node_pubkey: node,
        authorized_voter: voter,
        authorized_withdrawer: withdrawer,
        commission: 10,
    };
    let init_ix = nusantara_vote_program::initialize_account(&vote_acc, init);
    let init_msg = Message::new(&[init_ix], &vote_acc).unwrap();
    let mut init_tx = Transaction::new(init_msg);
    init_tx.sign(&[&vote_acc_kp]);

    let cache = ProgramCache::new(16);
    let result = execute_transaction(&init_tx, &storage, &sysvars, &fee_calc, 100, &cache, None, false);
    assert!(result.status.is_ok(), "init failed: {:?}", result.status);
    commit_deltas(&storage, &result, 100);

    // Verify initialized
    let loaded = storage.get_account(&vote_acc).unwrap().unwrap();
    let state = VoteState::try_from_slice(&loaded.data).unwrap();
    assert_eq!(state.commission, 10);
    assert_eq!(state.authorized_voter, voter);

    // Step 2: Cast votes
    let v = Vote {
        slots: vec![95, 96, 97, 98, 99],
        hash: hash(b"block_hash"),
        timestamp: Some(1_000_000),
    };
    let vote_ix = nusantara_vote_program::vote(&vote_acc, &voter, v);
    let vote_msg = Message::new(&[vote_ix], &voter).unwrap();
    let mut vote_tx = Transaction::new(vote_msg);
    vote_tx.sign(&[&voter_kp]);

    let result = execute_transaction(&vote_tx, &storage, &sysvars, &fee_calc, 101, &cache, None, false);
    assert!(result.status.is_ok(), "vote failed: {:?}", result.status);
    commit_deltas(&storage, &result, 101);

    let loaded = storage.get_account(&vote_acc).unwrap().unwrap();
    let state = VoteState::try_from_slice(&loaded.data).unwrap();
    assert_eq!(state.votes.len(), 5);

    // Step 3: Authorize new voter
    let new_voter = hash(b"new_voter");
    let auth_ix =
        nusantara_vote_program::authorize(&vote_acc, &voter, new_voter, VoteAuthorize::Voter);
    let auth_msg = Message::new(&[auth_ix], &voter).unwrap();
    let mut auth_tx = Transaction::new(auth_msg);
    auth_tx.sign(&[&voter_kp]);

    let result = execute_transaction(&auth_tx, &storage, &sysvars, &fee_calc, 102, &cache, None, false);
    assert!(result.status.is_ok(), "auth failed: {:?}", result.status);
    commit_deltas(&storage, &result, 102);

    let loaded = storage.get_account(&vote_acc).unwrap().unwrap();
    let state = VoteState::try_from_slice(&loaded.data).unwrap();
    assert_eq!(state.authorized_voter, new_voter);

    // Step 4: Withdraw
    let dest = hash(b"dest");
    let w_ix = nusantara_vote_program::withdraw(&vote_acc, &withdrawer, &dest, 100_000);
    let w_msg = Message::new(&[w_ix], &withdrawer).unwrap();
    let mut w_tx = Transaction::new(w_msg);
    w_tx.sign(&[&withdrawer_kp]);

    let result = execute_transaction(&w_tx, &storage, &sysvars, &fee_calc, 103, &cache, None, false);
    assert!(
        result.status.is_ok(),
        "withdraw failed: {:?}",
        result.status
    );
    commit_deltas(&storage, &result, 103);

    let dest_acc = storage.get_account(&dest).unwrap().unwrap();
    assert_eq!(dest_acc.lamports, 100_000);
}
