use nusantara_core::program::SYSTEM_PROGRAM_ID;
use nusantara_core::{Account, EpochSchedule, FeeCalculator, Message, Transaction};
use nusantara_crypto::{Hash, Keypair, hash};
use nusantara_rent_program::Rent;
use nusantara_runtime::{ProgramCache, SysvarCache, execute_transaction};
use nusantara_storage::Storage;
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
fn create_account_and_verify_storage() {
    let (storage, _dir) = test_storage();
    let funder_kp = Keypair::generate();
    let funder = funder_kp.address();
    let new_acc_kp = Keypair::generate();
    let new_acc = new_acc_kp.address();
    let owner = hash(b"owner_program");
    let rent = Rent::default();
    let min = rent.minimum_balance(100);

    storage
        .put_account(
            &funder,
            0,
            &Account::new(min + 1_000_000, *SYSTEM_PROGRAM_ID),
        )
        .unwrap();

    let ix = nusantara_system_program::create_account(&funder, &new_acc, min, 100, &owner);
    let msg = Message::new(&[ix], &funder).unwrap();
    let mut tx = Transaction::new(msg);
    tx.sign(&[&funder_kp, &new_acc_kp]);
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

    // Commit deltas
    for (addr, account) in &result.account_deltas {
        storage.put_account(addr, 1, account).unwrap();
    }

    // Verify new account in storage
    let loaded = storage.get_account(&new_acc).unwrap().unwrap();
    assert_eq!(loaded.lamports, min);
    assert_eq!(loaded.owner, owner);
    assert_eq!(loaded.data.len(), 100);

    // Verify funder balance decreased
    let loaded_funder = storage.get_account(&funder).unwrap().unwrap();
    // create_account requires 2 signers (funder + new_acc), so fee = 5000 * 2
    assert_eq!(loaded_funder.lamports, min + 1_000_000 - min - 10_000);
}

#[test]
fn transfer_and_verify_balances() {
    let (storage, _dir) = test_storage();
    let alice_kp = Keypair::generate();
    let alice = alice_kp.address();
    let bob = hash(b"bob");

    storage
        .put_account(&alice, 0, &Account::new(1_000_000, *SYSTEM_PROGRAM_ID))
        .unwrap();
    storage
        .put_account(&bob, 0, &Account::new(500_000, *SYSTEM_PROGRAM_ID))
        .unwrap();

    let ix = nusantara_system_program::transfer(&alice, &bob, 200_000);
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

    for (addr, account) in &result.account_deltas {
        storage.put_account(addr, 1, account).unwrap();
    }

    let alice_acc = storage.get_account(&alice).unwrap().unwrap();
    assert_eq!(alice_acc.lamports, 1_000_000 - 200_000 - 5000);

    let bob_acc = storage.get_account(&bob).unwrap().unwrap();
    assert_eq!(bob_acc.lamports, 700_000);
}

#[test]
fn rent_enforcement_on_create() {
    let (storage, _dir) = test_storage();
    let funder_kp = Keypair::generate();
    let funder = funder_kp.address();
    let new_acc_kp = Keypair::generate();
    let new_acc = new_acc_kp.address();
    let owner = hash(b"owner");

    storage
        .put_account(&funder, 0, &Account::new(10_000_000, *SYSTEM_PROGRAM_ID))
        .unwrap();

    // Try to create with lamports below rent minimum
    let ix = nusantara_system_program::create_account(&funder, &new_acc, 100, 1000, &owner);
    let msg = Message::new(&[ix], &funder).unwrap();
    let mut tx = Transaction::new(msg);
    tx.sign(&[&funder_kp, &new_acc_kp]);
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
}

#[test]
fn multiple_transfers_sequential() {
    let (storage, _dir) = test_storage();
    let alice_kp = Keypair::generate();
    let alice = alice_kp.address();
    let bob = hash(b"bob");
    let carol = hash(b"carol");

    storage
        .put_account(&alice, 0, &Account::new(5_000_000, *SYSTEM_PROGRAM_ID))
        .unwrap();

    let ix1 = nusantara_system_program::transfer(&alice, &bob, 100_000);
    let ix2 = nusantara_system_program::transfer(&alice, &carol, 200_000);
    let msg = Message::new(&[ix1, ix2], &alice).unwrap();
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

    for (addr, account) in &result.account_deltas {
        storage.put_account(addr, 1, account).unwrap();
    }

    let alice_acc = storage.get_account(&alice).unwrap().unwrap();
    assert_eq!(alice_acc.lamports, 5_000_000 - 100_000 - 200_000 - 5000);
}
