use nusantara_core::{Block, BlockHeader};
use nusantara_crypto::hash;
use nusantara_storage::{
    DataShred, CodeShred, SlotMeta, SnapshotManifest, Storage,
    TransactionStatus, TransactionStatusMeta,
};

fn temp_storage() -> (Storage, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path()).unwrap();
    (storage, dir)
}

fn test_header(slot: u64) -> BlockHeader {
    BlockHeader {
        slot,
        parent_slot: slot.saturating_sub(1),
        parent_hash: hash(b"parent"),
        block_hash: hash(format!("block_{slot}").as_bytes()),
        timestamp: 1000 + slot as i64,
        validator: hash(b"validator"),
        transaction_count: 10,
        merkle_root: hash(b"merkle"),
        poh_hash: nusantara_crypto::Hash::zero(),
        bank_hash: nusantara_crypto::Hash::zero(),
        state_root: nusantara_crypto::Hash::zero(),
    }
}

// --- Block Header Tests ---

#[test]
fn block_headers_range_query() {
    let (storage, _dir) = temp_storage();

    // Store blocks at non-contiguous slots
    for slot in [1, 5, 10, 15, 20, 25] {
        storage.put_block_header(&test_header(slot)).unwrap();
    }

    let range = storage.get_block_headers_range(5, 20).unwrap();
    assert_eq!(range.len(), 4); // slots 5, 10, 15, 20
    assert_eq!(range[0].slot, 5);
    assert_eq!(range[3].slot, 20);

    // Empty range
    let empty = storage.get_block_headers_range(2, 4).unwrap();
    assert!(empty.is_empty());
}

#[test]
fn latest_slot_tracks_highest() {
    let (storage, _dir) = temp_storage();
    assert!(storage.get_latest_slot().unwrap().is_none());

    storage.put_block_header(&test_header(100)).unwrap();
    assert_eq!(storage.get_latest_slot().unwrap(), Some(100));

    storage.put_block_header(&test_header(50)).unwrap();
    assert_eq!(storage.get_latest_slot().unwrap(), Some(100));

    storage.put_block_header(&test_header(200)).unwrap();
    assert_eq!(storage.get_latest_slot().unwrap(), Some(200));
}

#[test]
fn block_with_header_roundtrip() {
    let (storage, _dir) = temp_storage();
    let block = Block {
        header: test_header(42),
        transactions: Vec::new(),
        batches: Vec::new(),
    };
    storage.put_block(&block).unwrap();
    let loaded = storage.get_block_header(42).unwrap().unwrap();
    assert_eq!(loaded, block.header);
}

// --- Transaction Tests ---

#[test]
fn transaction_status_success_and_failure() {
    let (storage, _dir) = temp_storage();

    let tx_ok = hash(b"tx_ok");
    let meta_ok = TransactionStatusMeta {
        slot: 1,
        status: TransactionStatus::Success,
        fee: 5000,
        pre_balances: vec![1_000_000, 500_000],
        post_balances: vec![994_000, 501_000],
        compute_units_consumed: 200,
    };
    storage.put_transaction_status(&tx_ok, &meta_ok).unwrap();

    let tx_fail = hash(b"tx_fail");
    let meta_fail = TransactionStatusMeta {
        slot: 1,
        status: TransactionStatus::Failed("insufficient funds".into()),
        fee: 5000,
        pre_balances: vec![100],
        post_balances: vec![100],
        compute_units_consumed: 50,
    };
    storage.put_transaction_status(&tx_fail, &meta_fail).unwrap();

    let loaded_ok = storage.get_transaction_status(&tx_ok).unwrap().unwrap();
    assert_eq!(loaded_ok.status, TransactionStatus::Success);

    let loaded_fail = storage.get_transaction_status(&tx_fail).unwrap().unwrap();
    assert!(matches!(loaded_fail.status, TransactionStatus::Failed(ref msg) if msg == "insufficient funds"));
}

#[test]
fn address_signatures_multi_slot() {
    let (storage, _dir) = temp_storage();
    let addr = hash(b"alice");

    // Store 50 transactions across 10 slots
    for slot in 1..=10u64 {
        for tx_idx in 0..5u32 {
            let tx = hash(format!("tx_s{slot}_i{tx_idx}").as_bytes());
            storage
                .put_address_signature(&addr, slot, tx_idx, &tx)
                .unwrap();
        }
    }

    let all = storage.get_signatures_for_address(&addr, 100).unwrap();
    assert_eq!(all.len(), 50);

    // First result should be highest slot, highest tx_index
    assert_eq!(all[0].0, 10); // slot
    assert_eq!(all[0].1, 4); // tx_index

    // Limit works
    let limited = storage.get_signatures_for_address(&addr, 7).unwrap();
    assert_eq!(limited.len(), 7);
}

#[test]
fn address_signatures_isolation_between_addresses() {
    let (storage, _dir) = temp_storage();
    let addr_a = hash(b"addr_a");
    let addr_b = hash(b"addr_b");

    for i in 0..5u32 {
        let tx_a = hash(format!("tx_a_{i}").as_bytes());
        let tx_b = hash(format!("tx_b_{i}").as_bytes());
        storage.put_address_signature(&addr_a, 1, i, &tx_a).unwrap();
        storage.put_address_signature(&addr_b, 1, i, &tx_b).unwrap();
    }

    let sigs_a = storage.get_signatures_for_address(&addr_a, 100).unwrap();
    let sigs_b = storage.get_signatures_for_address(&addr_b, 100).unwrap();
    assert_eq!(sigs_a.len(), 5);
    assert_eq!(sigs_b.len(), 5);

    // Verify no cross-contamination
    for (_, _, tx_hash) in &sigs_a {
        assert!(sigs_b.iter().all(|(_, _, h)| h != tx_hash));
    }
}

// --- Shred Tests ---

#[test]
fn data_shreds_for_multiple_slots() {
    let (storage, _dir) = temp_storage();

    // Store shreds across 3 slots
    for slot in [1, 2, 3] {
        for idx in 0..10u32 {
            let shred = DataShred {
                slot,
                index: idx,
                parent_offset: 1,
                data: vec![idx as u8; 128],
                flags: 0,
            };
            storage.put_data_shred(&shred).unwrap();
        }
    }

    for slot in [1, 2, 3] {
        let shreds = storage.get_data_shreds_for_slot(slot).unwrap();
        assert_eq!(shreds.len(), 10);
        for (i, s) in shreds.iter().enumerate() {
            assert_eq!(s.index, i as u32);
            assert_eq!(s.slot, slot);
        }
    }

    // Non-existent slot
    assert!(storage.get_data_shreds_for_slot(99).unwrap().is_empty());
}

#[test]
fn code_shreds_roundtrip() {
    let (storage, _dir) = temp_storage();

    for idx in 0..5u32 {
        let shred = CodeShred {
            slot: 42,
            index: idx,
            num_data_shreds: 10,
            num_code_shreds: 5,
            position: idx,
            data: vec![0xAB; 64],
        };
        storage.put_code_shred(&shred).unwrap();
    }

    let shreds = storage.get_code_shreds_for_slot(42).unwrap();
    assert_eq!(shreds.len(), 5);
    for (i, s) in shreds.iter().enumerate() {
        assert_eq!(s.position, i as u32);
    }
}

// --- Slot Meta Tests ---

#[test]
fn slot_meta_roundtrip() {
    let (storage, _dir) = temp_storage();

    let meta = SlotMeta {
        slot: 100,
        parent_slot: 99,
        block_time: Some(1234567890),
        num_data_shreds: 32,
        num_code_shreds: 16,
        is_connected: true,
        completed: true,
    };
    storage.put_slot_meta(&meta).unwrap();

    let loaded = storage.get_slot_meta(100).unwrap().unwrap();
    assert_eq!(loaded, meta);
}

#[test]
fn slot_meta_incomplete() {
    let (storage, _dir) = temp_storage();

    let meta = SlotMeta {
        slot: 1,
        parent_slot: 0,
        block_time: None,
        num_data_shreds: 5,
        num_code_shreds: 0,
        is_connected: false,
        completed: false,
    };
    storage.put_slot_meta(&meta).unwrap();

    let loaded = storage.get_slot_meta(1).unwrap().unwrap();
    assert!(!loaded.is_connected);
    assert!(!loaded.completed);
    assert!(loaded.block_time.is_none());
}

// --- Snapshot Tests ---

#[test]
fn snapshot_latest_tracking() {
    let (storage, _dir) = temp_storage();
    assert!(storage.get_latest_snapshot().unwrap().is_none());

    for slot in [100, 200, 150, 300, 250] {
        let manifest = SnapshotManifest {
            slot,
            bank_hash: hash(format!("bank_{slot}").as_bytes()),
            account_count: slot + 1000,
            timestamp: slot as i64,
        };
        storage.put_snapshot(&manifest).unwrap();
    }

    let latest = storage.get_latest_snapshot().unwrap().unwrap();
    assert_eq!(latest.slot, 300);
}
