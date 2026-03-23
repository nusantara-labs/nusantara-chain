use nusantara_core::Account;
use nusantara_crypto::hash;
use nusantara_storage::Storage;

fn temp_storage() -> (Storage, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path()).unwrap();
    (storage, dir)
}

fn test_account(lamports: u64) -> Account {
    Account::new(lamports, hash(b"system"))
}

#[test]
fn account_versioning_latest_pointer_updates() {
    let (storage, _dir) = temp_storage();
    let addr = hash(b"alice");

    // Store three versions at different slots
    storage.put_account(&addr, 1, &test_account(100)).unwrap();
    assert_eq!(storage.get_account(&addr).unwrap().unwrap().lamports, 100);

    storage.put_account(&addr, 5, &test_account(500)).unwrap();
    assert_eq!(storage.get_account(&addr).unwrap().unwrap().lamports, 500);

    storage.put_account(&addr, 10, &test_account(1000)).unwrap();
    assert_eq!(storage.get_account(&addr).unwrap().unwrap().lamports, 1000);

    // All historical versions still accessible
    assert_eq!(
        storage.get_account_at_slot(&addr, 1).unwrap().unwrap().lamports,
        100
    );
    assert_eq!(
        storage.get_account_at_slot(&addr, 5).unwrap().unwrap().lamports,
        500
    );
    assert_eq!(
        storage.get_account_at_slot(&addr, 10).unwrap().unwrap().lamports,
        1000
    );
}

#[test]
fn account_history_returns_descending_order() {
    let (storage, _dir) = temp_storage();
    let addr = hash(b"bob");

    let slots = [2, 7, 3, 10, 1, 8];
    for &slot in &slots {
        storage
            .put_account(&addr, slot, &test_account(slot * 100))
            .unwrap();
    }

    let history = storage.get_account_history(&addr, 100).unwrap();
    assert_eq!(history.len(), slots.len());

    // Must be descending by slot
    for window in history.windows(2) {
        assert!(window[0].0 > window[1].0, "history not descending");
    }
}

#[test]
fn many_accounts_isolated_history() {
    let (storage, _dir) = temp_storage();

    // Create 20 accounts with 5 versions each
    let addrs: Vec<_> = (0..20).map(|i| hash(format!("user_{i}").as_bytes())).collect();
    for (i, addr) in addrs.iter().enumerate() {
        for slot in 1..=5 {
            let lamports = (i as u64 + 1) * 1000 + slot;
            storage
                .put_account(addr, slot, &test_account(lamports))
                .unwrap();
        }
    }

    // Each account's history should contain exactly 5 entries
    for (i, addr) in addrs.iter().enumerate() {
        let history = storage.get_account_history(addr, 100).unwrap();
        assert_eq!(history.len(), 5, "account {i} has wrong history length");

        // Latest should reflect slot 5
        let latest = storage.get_account(addr).unwrap().unwrap();
        assert_eq!(latest.lamports, (i as u64 + 1) * 1000 + 5);
    }
}

#[test]
fn account_overwrite_at_same_slot() {
    let (storage, _dir) = temp_storage();
    let addr = hash(b"carol");

    storage.put_account(&addr, 1, &test_account(100)).unwrap();
    storage.put_account(&addr, 1, &test_account(200)).unwrap();

    // Should reflect the overwritten value
    let loaded = storage.get_account_at_slot(&addr, 1).unwrap().unwrap();
    assert_eq!(loaded.lamports, 200);

    // History should have 1 entry (same key overwritten)
    let history = storage.get_account_history(&addr, 100).unwrap();
    assert_eq!(history.len(), 1);
}

#[test]
fn account_persistence_across_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let addr = hash(b"persist");

    {
        let storage = Storage::open(dir.path()).unwrap();
        storage
            .put_account(&addr, 1, &test_account(42))
            .unwrap();
        storage
            .put_account(&addr, 5, &test_account(99))
            .unwrap();
    }

    // Reopen
    let storage = Storage::open(dir.path()).unwrap();
    let latest = storage.get_account(&addr).unwrap().unwrap();
    assert_eq!(latest.lamports, 99);

    let at_1 = storage.get_account_at_slot(&addr, 1).unwrap().unwrap();
    assert_eq!(at_1.lamports, 42);

    let history = storage.get_account_history(&addr, 10).unwrap();
    assert_eq!(history.len(), 2);
}

#[test]
fn account_history_with_limit_one() {
    let (storage, _dir) = temp_storage();
    let addr = hash(b"limited");

    for slot in 1..=5 {
        storage.put_account(&addr, slot, &test_account(slot * 100)).unwrap();
    }
    let history = storage.get_account_history(&addr, 1).unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].0, 5); // most recent slot
}

#[test]
fn nonexistent_account_returns_none() {
    let (storage, _dir) = temp_storage();
    let addr = hash(b"ghost");

    assert!(storage.get_account(&addr).unwrap().is_none());
    assert!(storage.get_account_at_slot(&addr, 42).unwrap().is_none());
    assert!(storage.get_account_history(&addr, 10).unwrap().is_empty());
}
