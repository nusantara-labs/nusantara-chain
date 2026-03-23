use nusantara_crypto::hash;
use nusantara_storage::Storage;
use nusantara_sysvar_program::Clock;

fn temp_storage() -> (Storage, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path()).unwrap();
    (storage, dir)
}

// --- Root Management ---

#[test]
fn roots_track_finalized_slots() {
    let (storage, _dir) = temp_storage();

    // No roots initially
    assert!(storage.get_latest_root().unwrap().is_none());

    // Set roots in non-sequential order
    for slot in [5, 10, 3, 20, 15] {
        storage.set_root(slot).unwrap();
    }

    // All should be roots
    for slot in [5, 10, 3, 20, 15] {
        assert!(storage.is_root(slot).unwrap());
    }

    // Non-roots
    for slot in [1, 2, 4, 6, 7, 8, 9, 11, 50] {
        assert!(!storage.is_root(slot).unwrap());
    }

    // Latest root is the highest
    assert_eq!(storage.get_latest_root().unwrap(), Some(20));
}

#[test]
fn roots_idempotent() {
    let (storage, _dir) = temp_storage();

    storage.set_root(42).unwrap();
    storage.set_root(42).unwrap();
    storage.set_root(42).unwrap();

    assert!(storage.is_root(42).unwrap());
}

// --- Bank Hash Management ---

#[test]
fn bank_hashes_per_slot() {
    let (storage, _dir) = temp_storage();

    let slots_and_hashes: Vec<_> = (0..10)
        .map(|i| (i, hash(format!("bank_{i}").as_bytes())))
        .collect();

    for &(slot, ref h) in &slots_and_hashes {
        storage.put_bank_hash(slot, h).unwrap();
    }

    for &(slot, ref expected) in &slots_and_hashes {
        let loaded = storage.get_bank_hash(slot).unwrap().unwrap();
        assert_eq!(&loaded, expected);
    }

    // Missing slot
    assert!(storage.get_bank_hash(999).unwrap().is_none());
}

// --- Slot Hash Management ---

#[test]
fn slot_hashes_per_slot() {
    let (storage, _dir) = temp_storage();

    for slot in 0..10u64 {
        let h = hash(format!("slot_{slot}").as_bytes());
        storage.put_slot_hash(slot, &h).unwrap();
    }

    for slot in 0..10u64 {
        let expected = hash(format!("slot_{slot}").as_bytes());
        let loaded = storage.get_slot_hash(slot).unwrap().unwrap();
        assert_eq!(loaded, expected);
    }

    assert!(storage.get_slot_hash(999).unwrap().is_none());
}

// --- Sysvar Storage ---

#[test]
fn sysvar_store_and_retrieve() {
    let (storage, _dir) = temp_storage();

    let clock = Clock {
        slot: 100,
        epoch: 2,
        unix_timestamp: 1234567890,
        leader_schedule_epoch: 3,
        epoch_start_timestamp: 1234500000,
    };

    storage.put_sysvar(&clock).unwrap();
    let loaded: Clock = storage.get_sysvar::<Clock>().unwrap().unwrap();
    assert_eq!(loaded.slot, 100);
    assert_eq!(loaded.epoch, 2);
    assert_eq!(loaded.unix_timestamp, 1234567890);
}

#[test]
fn sysvar_update_overwrites() {
    let (storage, _dir) = temp_storage();

    let clock_v1 = Clock {
        slot: 1,
        epoch: 0,
        unix_timestamp: 1000,
        leader_schedule_epoch: 1,
        epoch_start_timestamp: 900,
    };
    storage.put_sysvar(&clock_v1).unwrap();

    let clock_v2 = Clock {
        slot: 500,
        epoch: 3,
        unix_timestamp: 5000,
        leader_schedule_epoch: 4,
        epoch_start_timestamp: 4900,
    };
    storage.put_sysvar(&clock_v2).unwrap();

    let loaded: Clock = storage.get_sysvar::<Clock>().unwrap().unwrap();
    assert_eq!(loaded.slot, 500);
    assert_eq!(loaded.epoch, 3);
}

// --- Cross-module consistency ---

#[test]
fn full_slot_lifecycle() {
    let (storage, _dir) = temp_storage();
    let slot = 42u64;

    // 1. Store block header
    let header = nusantara_core::BlockHeader {
        slot,
        parent_slot: 41,
        parent_hash: hash(b"parent_41"),
        block_hash: hash(b"block_42"),
        timestamp: 1000,
        validator: hash(b"validator"),
        transaction_count: 3,
        merkle_root: hash(b"merkle_42"),
        poh_hash: nusantara_crypto::Hash::zero(),
        bank_hash: nusantara_crypto::Hash::zero(),
        state_root: nusantara_crypto::Hash::zero(),
    };
    storage.put_block_header(&header).unwrap();

    // 2. Store slot meta
    let meta = nusantara_storage::SlotMeta {
        slot,
        parent_slot: 41,
        block_time: Some(1000),
        num_data_shreds: 10,
        num_code_shreds: 5,
        is_connected: true,
        completed: true,
    };
    storage.put_slot_meta(&meta).unwrap();

    // 3. Store bank hash
    let bank_hash = hash(b"bank_42");
    storage.put_bank_hash(slot, &bank_hash).unwrap();

    // 4. Store slot hash
    let slot_hash = hash(b"slot_42");
    storage.put_slot_hash(slot, &slot_hash).unwrap();

    // 5. Mark as root
    storage.set_root(slot).unwrap();

    // Verify all state is consistent
    assert_eq!(storage.get_block_header(slot).unwrap().unwrap(), header);
    assert_eq!(storage.get_slot_meta(slot).unwrap().unwrap(), meta);
    assert_eq!(storage.get_bank_hash(slot).unwrap().unwrap(), bank_hash);
    assert_eq!(storage.get_slot_hash(slot).unwrap().unwrap(), slot_hash);
    assert!(storage.is_root(slot).unwrap());
}

#[test]
fn persistence_across_reopen() {
    let dir = tempfile::tempdir().unwrap();

    {
        let storage = Storage::open(dir.path()).unwrap();
        storage.set_root(10).unwrap();
        storage.set_root(20).unwrap();
        storage
            .put_bank_hash(10, &hash(b"bank_10"))
            .unwrap();
        storage
            .put_slot_hash(10, &hash(b"slot_10"))
            .unwrap();
        let clock = Clock {
            slot: 20,
            epoch: 1,
            unix_timestamp: 2000,
            leader_schedule_epoch: 2,
            epoch_start_timestamp: 1900,
        };
        storage.put_sysvar(&clock).unwrap();
    }

    // Reopen and verify
    let storage = Storage::open(dir.path()).unwrap();
    assert!(storage.is_root(10).unwrap());
    assert!(storage.is_root(20).unwrap());
    assert!(!storage.is_root(15).unwrap());
    assert_eq!(storage.get_latest_root().unwrap(), Some(20));
    assert_eq!(
        storage.get_bank_hash(10).unwrap().unwrap(),
        hash(b"bank_10")
    );
    assert_eq!(
        storage.get_slot_hash(10).unwrap().unwrap(),
        hash(b"slot_10")
    );
    let clock: Clock = storage.get_sysvar::<Clock>().unwrap().unwrap();
    assert_eq!(clock.slot, 20);
}
