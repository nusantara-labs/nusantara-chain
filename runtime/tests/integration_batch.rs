use nusantara_core::program::SYSTEM_PROGRAM_ID;
use nusantara_core::{Account, EpochSchedule, FeeCalculator, Message, Transaction};
use nusantara_crypto::{Hash, Keypair, hash};
use nusantara_rent_program::Rent;
use nusantara_runtime::{ProgramCache, SysvarCache, execute_slot};
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

fn transfer_tx(from_kp: &Keypair, to: Hash, amount: u64) -> Transaction {
    let from = from_kp.address();
    let ix = nusantara_system_program::transfer(&from, &to, amount);
    let msg = Message::new(&[ix], &from).unwrap();
    let mut tx = Transaction::new(msg);
    tx.sign(&[from_kp]);
    tx
}

#[test]
fn batch_execution_with_state_deps() {
    let (storage, _dir) = test_storage();
    let alice_kp = Keypair::generate();
    let alice = alice_kp.address();
    let bob = hash(b"bob");
    let carol = hash(b"carol");
    let fee_calc = FeeCalculator::default();
    let sysvars = test_sysvars();

    // Fund alice
    storage
        .put_account(&alice, 0, &Account::new(5_000_000, *SYSTEM_PROGRAM_ID))
        .unwrap();

    // tx1: alice -> bob 100k
    // tx2: alice -> carol 200k (depends on alice's state after tx1)
    let tx1 = transfer_tx(&alice_kp, bob, 100_000);
    let tx2 = transfer_tx(&alice_kp, carol, 200_000);

    let cache = ProgramCache::new(16);
    let result = execute_slot(1, &[tx1, tx2], &storage, &sysvars, &fee_calc, &cache).unwrap();

    assert_eq!(result.slot, 1);
    assert_eq!(result.transactions_executed, 2);
    assert_eq!(result.transactions_succeeded, 2);
    assert_eq!(result.total_fees, 10_000); // 5000 * 2

    // Verify alice's final balance
    let alice_acc = storage.get_account(&alice).unwrap().unwrap();
    assert_eq!(alice_acc.lamports, 5_000_000 - 100_000 - 200_000 - 10_000);

    // Verify bob and carol received
    let bob_acc = storage.get_account(&bob).unwrap().unwrap();
    assert_eq!(bob_acc.lamports, 100_000);

    let carol_acc = storage.get_account(&carol).unwrap().unwrap();
    assert_eq!(carol_acc.lamports, 200_000);
}

#[test]
fn batch_mixed_success_failure() {
    let (storage, _dir) = test_storage();
    let alice_kp = Keypair::generate();
    let alice = alice_kp.address();
    let bob = hash(b"bob");
    let poor_kp = Keypair::generate();
    let poor = poor_kp.address();
    let carol = hash(b"carol");
    let fee_calc = FeeCalculator::default();
    let sysvars = test_sysvars();

    storage
        .put_account(&alice, 0, &Account::new(1_000_000, *SYSTEM_PROGRAM_ID))
        .unwrap();
    storage
        .put_account(&poor, 0, &Account::new(10_000, *SYSTEM_PROGRAM_ID))
        .unwrap();

    let tx1 = transfer_tx(&alice_kp, bob, 100_000);
    let tx2 = transfer_tx(&poor_kp, carol, 1_000_000); // will fail

    let cache = ProgramCache::new(16);
    let result = execute_slot(1, &[tx1, tx2], &storage, &sysvars, &fee_calc, &cache).unwrap();

    assert_eq!(result.transactions_succeeded, 1);
    assert_eq!(result.transactions_failed, 1);
    assert_eq!(result.total_fees, 10_000); // both pay fees

    // Verify tx status stored
    let alice_sigs = storage.get_signatures_for_address(&alice, 10).unwrap();
    assert!(!alice_sigs.is_empty());
}

#[test]
fn delta_hash_determinism() {
    let (storage1, _dir1) = test_storage();
    let (storage2, _dir2) = test_storage();
    let alice_kp = Keypair::generate();
    let alice = alice_kp.address();
    let bob = hash(b"bob");
    let fee_calc = FeeCalculator::default();
    let sysvars = test_sysvars();

    for s in [&storage1, &storage2] {
        s.put_account(&alice, 0, &Account::new(1_000_000, *SYSTEM_PROGRAM_ID))
            .unwrap();
    }

    let tx = transfer_tx(&alice_kp, bob, 100_000);

    let cache = ProgramCache::new(16);
    let r1 = execute_slot(
        1,
        std::slice::from_ref(&tx),
        &storage1,
        &sysvars,
        &fee_calc,
        &cache,
    )
    .unwrap();
    let r2 = execute_slot(
        1,
        std::slice::from_ref(&tx),
        &storage2,
        &sysvars,
        &fee_calc,
        &cache,
    )
    .unwrap();

    assert_eq!(r1.account_delta_hash, r2.account_delta_hash);
    assert_eq!(r1.total_fees, r2.total_fees);
    assert_eq!(r1.total_compute_consumed, r2.total_compute_consumed);
}

#[test]
fn empty_slot_execution() {
    let (storage, _dir) = test_storage();
    let fee_calc = FeeCalculator::default();
    let sysvars = test_sysvars();

    let cache = ProgramCache::new(16);
    let result = execute_slot(42, &[], &storage, &sysvars, &fee_calc, &cache).unwrap();

    assert_eq!(result.slot, 42);
    assert_eq!(result.transactions_executed, 0);
    assert_eq!(result.transactions_succeeded, 0);
    assert_eq!(result.transactions_failed, 0);
    assert_eq!(result.total_fees, 0);
    assert_eq!(result.total_compute_consumed, 0);
}

#[test]
fn transaction_status_meta_stored() {
    let (storage, _dir) = test_storage();
    let alice_kp = Keypair::generate();
    let alice = alice_kp.address();
    let bob = hash(b"bob");
    let fee_calc = FeeCalculator::default();
    let sysvars = test_sysvars();

    storage
        .put_account(&alice, 0, &Account::new(1_000_000, *SYSTEM_PROGRAM_ID))
        .unwrap();

    let tx = transfer_tx(&alice_kp, bob, 100_000);
    let tx_hash = tx.hash();

    let cache = ProgramCache::new(16);
    execute_slot(1, &[tx], &storage, &sysvars, &fee_calc, &cache).unwrap();

    let meta = storage.get_transaction_status(&tx_hash).unwrap().unwrap();
    assert_eq!(meta.slot, 1);
    assert_eq!(meta.fee, 5000);
    assert!(matches!(meta.status, TransactionStatus::Success));
    assert!(meta.compute_units_consumed > 0);
}
