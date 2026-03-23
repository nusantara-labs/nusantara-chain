use nusantara_core::program::SYSTEM_PROGRAM_ID;
use nusantara_core::{Account, EpochSchedule, FeeCalculator, Message, Transaction};
use nusantara_crypto::{Hash, Keypair, hash};
use nusantara_rent_program::Rent;
use nusantara_runtime::{ProgramCache, SysvarCache, execute_transaction};
use nusantara_storage::{Storage, TransactionStatus};
use nusantara_sysvar_program::{Clock, RecentBlockhashes, SlotHashes, StakeHistory};
use tempfile::tempdir;

fn test_sysvars() -> SysvarCache {
    SysvarCache::new(
        Clock::default(),
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

#[test]
fn full_pipeline_success() {
    let (storage, _dir) = test_storage();
    let alice_kp = Keypair::generate();
    let alice = alice_kp.address();
    let bob = hash(b"bob");

    storage
        .put_account(&alice, 0, &Account::new(1_000_000, *SYSTEM_PROGRAM_ID))
        .unwrap();

    let ix = nusantara_system_program::transfer(&alice, &bob, 100_000);
    let msg = Message::new(&[ix], &alice).unwrap();
    let mut tx = Transaction::new(msg);
    tx.sign(&[&alice_kp]);
    let fee_calc = FeeCalculator::default();
    let sysvars = test_sysvars();

    let result = execute_transaction(
        &tx,
        &storage,
        &sysvars,
        &fee_calc,
        1,
        &ProgramCache::new(16),
        None,
        false,
    );
    assert!(result.status.is_ok());
    assert_eq!(result.fee, 5000);
    assert!(result.compute_units_consumed > 0);

    // Verify pre/post balances
    let alice_idx = result
        .pre_balances
        .iter()
        .position(|b| *b == 1_000_000)
        .unwrap();
    assert!(result.post_balances[alice_idx] < 1_000_000);
}

#[test]
fn transaction_status_meta_recording() {
    let (storage, _dir) = test_storage();
    let alice_kp = Keypair::generate();
    let alice = alice_kp.address();
    let bob = hash(b"bob");

    storage
        .put_account(&alice, 0, &Account::new(1_000_000, *SYSTEM_PROGRAM_ID))
        .unwrap();

    let ix = nusantara_system_program::transfer(&alice, &bob, 100_000);
    let msg = Message::new(&[ix], &alice).unwrap();
    let mut tx = Transaction::new(msg);
    tx.sign(&[&alice_kp]);
    let fee_calc = FeeCalculator::default();
    let sysvars = test_sysvars();

    let result = execute_transaction(
        &tx,
        &storage,
        &sysvars,
        &fee_calc,
        1,
        &ProgramCache::new(16),
        None,
        false,
    );

    // Manually store the meta as batch_executor would
    let meta = nusantara_storage::TransactionStatusMeta {
        slot: 1,
        status: TransactionStatus::Success,
        fee: result.fee,
        pre_balances: result.pre_balances.clone(),
        post_balances: result.post_balances.clone(),
        compute_units_consumed: result.compute_units_consumed,
    };
    storage
        .put_transaction_status(&result.tx_hash, &meta)
        .unwrap();

    // Verify it was stored
    let loaded = storage
        .get_transaction_status(&result.tx_hash)
        .unwrap()
        .unwrap();
    assert_eq!(loaded.fee, 5000);
    assert!(matches!(loaded.status, TransactionStatus::Success));
}

#[test]
fn failure_modes() {
    let (storage, _dir) = test_storage();
    let poor_kp = Keypair::generate();
    let poor = poor_kp.address();
    let bob = hash(b"bob");

    // Insufficient balance for transfer
    storage
        .put_account(&poor, 0, &Account::new(10_000, *SYSTEM_PROGRAM_ID))
        .unwrap();

    let ix = nusantara_system_program::transfer(&poor, &bob, 1_000_000);
    let msg = Message::new(&[ix], &poor).unwrap();
    let mut tx = Transaction::new(msg);
    tx.sign(&[&poor_kp]);
    let fee_calc = FeeCalculator::default();
    let sysvars = test_sysvars();

    let result = execute_transaction(
        &tx,
        &storage,
        &sysvars,
        &fee_calc,
        1,
        &ProgramCache::new(16),
        None,
        false,
    );
    assert!(result.status.is_err());
    // Fee still charged
    assert_eq!(result.fee, 5000);
    // Payer balance should reflect fee deduction
    assert!(!result.account_deltas.is_empty());
}

#[test]
fn address_signature_recording() {
    let (storage, _dir) = test_storage();
    let alice_kp = Keypair::generate();
    let alice = alice_kp.address();
    let bob = hash(b"bob");

    storage
        .put_account(&alice, 0, &Account::new(1_000_000, *SYSTEM_PROGRAM_ID))
        .unwrap();

    let ix = nusantara_system_program::transfer(&alice, &bob, 100_000);
    let msg = Message::new(&[ix], &alice).unwrap();
    let mut tx = Transaction::new(msg);
    tx.sign(&[&alice_kp]);
    let fee_calc = FeeCalculator::default();
    let sysvars = test_sysvars();

    let result = execute_transaction(
        &tx,
        &storage,
        &sysvars,
        &fee_calc,
        1,
        &ProgramCache::new(16),
        None,
        false,
    );
    assert!(result.status.is_ok());

    // Commit and record
    for (addr, account) in &result.account_deltas {
        storage.put_account(addr, 1, account).unwrap();
        storage
            .put_address_signature(addr, 1, 0, &result.tx_hash)
            .unwrap();
    }

    // Verify alice has a signature record
    let sigs = storage.get_signatures_for_address(&alice, 10).unwrap();
    assert!(!sigs.is_empty());
    assert_eq!(sigs[0].2, result.tx_hash);
}

#[test]
fn blockhash_zero_allowed_at_genesis() {
    let (storage, _dir) = test_storage();
    let alice_kp = Keypair::generate();
    let alice = alice_kp.address();
    let bob = hash(b"bob");

    storage
        .put_account(&alice, 0, &Account::new(1_000_000, *SYSTEM_PROGRAM_ID))
        .unwrap();

    // Hash::zero() as blockhash should only be allowed at genesis (slot 0)
    let ix = nusantara_system_program::transfer(&alice, &bob, 100);
    let msg = Message::new(&[ix], &alice).unwrap();
    assert_eq!(msg.recent_blockhash, Hash::zero());

    let mut tx = Transaction::new(msg);
    tx.sign(&[&alice_kp]);
    let sysvars = SysvarCache::new(
        Clock::default(),
        Rent::default(),
        EpochSchedule::default(),
        SlotHashes::default(),
        StakeHistory::default(),
        RecentBlockhashes::new(vec![hash(b"some_blockhash")]),
    );
    let fee_calc = FeeCalculator::default();

    // At slot 0 (genesis): Hash::zero() is allowed
    let result = execute_transaction(
        &tx,
        &storage,
        &sysvars,
        &fee_calc,
        0,
        &ProgramCache::new(16),
        None,
        false,
    );
    assert!(result.status.is_ok());
}

#[test]
fn blockhash_zero_rejected_after_genesis() {
    let (storage, _dir) = test_storage();
    let alice_kp = Keypair::generate();
    let alice = alice_kp.address();
    let bob = hash(b"bob");

    storage
        .put_account(&alice, 0, &Account::new(1_000_000, *SYSTEM_PROGRAM_ID))
        .unwrap();

    let ix = nusantara_system_program::transfer(&alice, &bob, 100);
    let msg = Message::new(&[ix], &alice).unwrap();
    assert_eq!(msg.recent_blockhash, Hash::zero());

    let mut tx = Transaction::new(msg);
    tx.sign(&[&alice_kp]);
    let sysvars = SysvarCache::new(
        Clock::default(),
        Rent::default(),
        EpochSchedule::default(),
        SlotHashes::default(),
        StakeHistory::default(),
        RecentBlockhashes::new(vec![hash(b"some_blockhash")]),
    );
    let fee_calc = FeeCalculator::default();

    // At slot > 0: Hash::zero() is rejected (not in recent blockhashes)
    let result = execute_transaction(
        &tx,
        &storage,
        &sysvars,
        &fee_calc,
        1,
        &ProgramCache::new(16),
        None,
        false,
    );
    assert!(result.status.is_err());
    assert!(matches!(
        result.status.unwrap_err(),
        nusantara_runtime::RuntimeError::BlockhashNotFound
    ));
}
