use criterion::{criterion_group, criterion_main, BatchSize, Criterion};
use nusantara_core::{Account, BlockHeader};
use nusantara_crypto::{Hash, hash};
use nusantara_storage::{
    DataShred, SlotMeta, SnapshotManifest, Storage, StorageWriteBatch, TransactionStatus,
    TransactionStatusMeta,
};

fn temp_storage() -> (Storage, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path()).unwrap();
    (storage, dir)
}

fn test_account(lamports: u64) -> Account {
    Account::new(lamports, hash(b"system"))
}

fn bench_account_operations(c: &mut Criterion) {
    let mut group = c.benchmark_group("account_operations");

    group.bench_function("put_account", |b| {
        let (storage, _dir) = temp_storage();
        let addr = hash(b"bench_addr");
        let account = test_account(1_000_000);
        let mut slot = 0u64;
        b.iter(|| {
            slot += 1;
            storage.put_account(&addr, slot, &account).unwrap();
        });
    });

    group.bench_function("get_account_latest", |b| {
        let (storage, _dir) = temp_storage();
        let addr = hash(b"bench_addr");
        storage
            .put_account(&addr, 1, &test_account(1000))
            .unwrap();
        b.iter(|| {
            storage.get_account(&addr).unwrap();
        });
    });

    group.bench_function("get_account_at_slot", |b| {
        let (storage, _dir) = temp_storage();
        let addr = hash(b"bench_addr");
        for slot in 1..=100u64 {
            storage
                .put_account(&addr, slot, &test_account(slot))
                .unwrap();
        }
        b.iter(|| {
            storage.get_account_at_slot(&addr, 50).unwrap();
        });
    });

    group.bench_function("get_account_history_100", |b| {
        let (storage, _dir) = temp_storage();
        let addr = hash(b"bench_addr");
        for slot in 1..=100u64 {
            storage
                .put_account(&addr, slot, &test_account(slot))
                .unwrap();
        }
        b.iter(|| {
            storage.get_account_history(&addr, 10).unwrap();
        });
    });

    group.finish();
}

fn bench_block_operations(c: &mut Criterion) {
    let mut group = c.benchmark_group("block_operations");

    group.bench_function("put_block_header", |b| {
        let (storage, _dir) = temp_storage();
        let mut slot = 0u64;
        b.iter(|| {
            slot += 1;
            let header = BlockHeader {
                slot,
                parent_slot: slot - 1,
                parent_hash: hash(b"parent"),
                block_hash: hash(format!("block_{slot}").as_bytes()),
                timestamp: slot as i64,
                validator: hash(b"validator"),
                transaction_count: 100,
                merkle_root: hash(b"merkle"),
                poh_hash: Hash::zero(),
                bank_hash: Hash::zero(),
                state_root: Hash::zero(),
            };
            storage.put_block_header(&header).unwrap();
        });
    });

    group.bench_function("get_block_header", |b| {
        let (storage, _dir) = temp_storage();
        for slot in 1..=1000u64 {
            let header = BlockHeader {
                slot,
                parent_slot: slot - 1,
                parent_hash: hash(b"parent"),
                block_hash: hash(format!("block_{slot}").as_bytes()),
                timestamp: slot as i64,
                validator: hash(b"validator"),
                transaction_count: 100,
                merkle_root: hash(b"merkle"),
                poh_hash: Hash::zero(),
                bank_hash: Hash::zero(),
                state_root: Hash::zero(),
            };
            storage.put_block_header(&header).unwrap();
        }
        b.iter(|| {
            storage.get_block_header(500).unwrap();
        });
    });

    group.bench_function("get_block_headers_range_100", |b| {
        let (storage, _dir) = temp_storage();
        for slot in 1..=1000u64 {
            let header = BlockHeader {
                slot,
                parent_slot: slot - 1,
                parent_hash: hash(b"parent"),
                block_hash: hash(format!("block_{slot}").as_bytes()),
                timestamp: slot as i64,
                validator: hash(b"validator"),
                transaction_count: 100,
                merkle_root: hash(b"merkle"),
                poh_hash: Hash::zero(),
                bank_hash: Hash::zero(),
                state_root: Hash::zero(),
            };
            storage.put_block_header(&header).unwrap();
        }
        b.iter(|| {
            storage.get_block_headers_range(100, 200).unwrap();
        });
    });

    group.bench_function("get_latest_slot", |b| {
        let (storage, _dir) = temp_storage();
        for slot in 1..=1000u64 {
            let header = BlockHeader {
                slot,
                parent_slot: slot - 1,
                parent_hash: hash(b"parent"),
                block_hash: hash(format!("block_{slot}").as_bytes()),
                timestamp: slot as i64,
                validator: hash(b"validator"),
                transaction_count: 100,
                merkle_root: hash(b"merkle"),
                poh_hash: Hash::zero(),
                bank_hash: Hash::zero(),
                state_root: Hash::zero(),
            };
            storage.put_block_header(&header).unwrap();
        }
        b.iter(|| {
            storage.get_latest_slot().unwrap();
        });
    });

    group.finish();
}

fn bench_transaction_operations(c: &mut Criterion) {
    let mut group = c.benchmark_group("transaction_operations");

    group.bench_function("put_transaction_status", |b| {
        let (storage, _dir) = temp_storage();
        let mut counter = 0u64;
        b.iter(|| {
            counter += 1;
            let tx_hash = hash(format!("tx_{counter}").as_bytes());
            let meta = TransactionStatusMeta {
                slot: 1,
                status: TransactionStatus::Success,
                fee: 5000,
                pre_balances: vec![1_000_000, 500_000],
                post_balances: vec![994_000, 501_000],
                compute_units_consumed: 200,
            };
            storage.put_transaction_status(&tx_hash, &meta).unwrap();
        });
    });

    group.bench_function("get_transaction_status", |b| {
        let (storage, _dir) = temp_storage();
        let tx_hash = hash(b"bench_tx");
        let meta = TransactionStatusMeta {
            slot: 1,
            status: TransactionStatus::Success,
            fee: 5000,
            pre_balances: vec![1_000_000],
            post_balances: vec![995_000],
            compute_units_consumed: 200,
        };
        storage.put_transaction_status(&tx_hash, &meta).unwrap();
        b.iter(|| {
            storage.get_transaction_status(&tx_hash).unwrap();
        });
    });

    group.bench_function("put_address_signature", |b| {
        let (storage, _dir) = temp_storage();
        let addr = hash(b"bench_addr");
        let mut counter = 0u32;
        b.iter(|| {
            counter += 1;
            let tx = hash(format!("tx_{counter}").as_bytes());
            storage
                .put_address_signature(&addr, 1, counter, &tx)
                .unwrap();
        });
    });

    group.bench_function("get_signatures_for_address_1000", |b| {
        let (storage, _dir) = temp_storage();
        let addr = hash(b"bench_addr");
        for i in 0..1000u32 {
            let tx = hash(format!("tx_{i}").as_bytes());
            storage
                .put_address_signature(&addr, i as u64 / 10, i % 10, &tx)
                .unwrap();
        }
        b.iter(|| {
            storage.get_signatures_for_address(&addr, 50).unwrap();
        });
    });

    group.finish();
}

fn bench_shred_operations(c: &mut Criterion) {
    let mut group = c.benchmark_group("shred_operations");

    group.bench_function("put_data_shred", |b| {
        let (storage, _dir) = temp_storage();
        let mut counter = 0u32;
        b.iter(|| {
            counter += 1;
            let shred = DataShred {
                slot: 1,
                index: counter,
                parent_offset: 1,
                data: vec![0u8; 1024],
                flags: 0,
            };
            storage.put_data_shred(&shred).unwrap();
        });
    });

    group.bench_function("get_data_shred", |b| {
        let (storage, _dir) = temp_storage();
        for idx in 0..100u32 {
            let shred = DataShred {
                slot: 1,
                index: idx,
                parent_offset: 1,
                data: vec![0u8; 1024],
                flags: 0,
            };
            storage.put_data_shred(&shred).unwrap();
        }
        b.iter(|| {
            storage.get_data_shred(1, 50).unwrap();
        });
    });

    group.bench_function("get_data_shreds_for_slot_100", |b| {
        let (storage, _dir) = temp_storage();
        for idx in 0..100u32 {
            let shred = DataShred {
                slot: 1,
                index: idx,
                parent_offset: 1,
                data: vec![0u8; 1024],
                flags: 0,
            };
            storage.put_data_shred(&shred).unwrap();
        }
        b.iter(|| {
            storage.get_data_shreds_for_slot(1).unwrap();
        });
    });

    group.finish();
}

fn bench_consensus_state(c: &mut Criterion) {
    let mut group = c.benchmark_group("consensus_state");

    group.bench_function("set_root", |b| {
        let (storage, _dir) = temp_storage();
        let mut slot = 0u64;
        b.iter(|| {
            slot += 1;
            storage.set_root(slot).unwrap();
        });
    });

    group.bench_function("is_root", |b| {
        let (storage, _dir) = temp_storage();
        for slot in 0..1000u64 {
            storage.set_root(slot).unwrap();
        }
        b.iter(|| {
            storage.is_root(500).unwrap();
        });
    });

    group.bench_function("get_latest_root", |b| {
        let (storage, _dir) = temp_storage();
        for slot in 0..1000u64 {
            storage.set_root(slot).unwrap();
        }
        b.iter(|| {
            storage.get_latest_root().unwrap();
        });
    });

    group.bench_function("put_bank_hash", |b| {
        let (storage, _dir) = temp_storage();
        let h = hash(b"bank");
        let mut slot = 0u64;
        b.iter(|| {
            slot += 1;
            storage.put_bank_hash(slot, &h).unwrap();
        });
    });

    group.bench_function("get_bank_hash", |b| {
        let (storage, _dir) = temp_storage();
        let h = hash(b"bank_500");
        storage.put_bank_hash(500, &h).unwrap();
        b.iter(|| {
            storage.get_bank_hash(500).unwrap();
        });
    });

    group.finish();
}

fn bench_write_batch(c: &mut Criterion) {
    let mut group = c.benchmark_group("write_batch");

    for size in [10, 100, 1000] {
        group.bench_function(format!("batch_write_{size}"), |b| {
            b.iter_batched(
                || {
                    let (storage, dir) = temp_storage();
                    let mut batch = StorageWriteBatch::new();
                    for i in 0..size as u64 {
                        batch.put("roots", i.to_be_bytes().to_vec(), vec![]);
                    }
                    (storage, dir, batch)
                },
                |(storage, _dir, batch)| {
                    storage.write(&batch).unwrap();
                },
                BatchSize::SmallInput,
            );
        });
    }

    group.finish();
}

fn bench_slot_meta(c: &mut Criterion) {
    let mut group = c.benchmark_group("slot_meta");

    group.bench_function("put_slot_meta", |b| {
        let (storage, _dir) = temp_storage();
        let mut slot = 0u64;
        b.iter(|| {
            slot += 1;
            let meta = SlotMeta {
                slot,
                parent_slot: slot - 1,
                block_time: Some(slot as i64),
                num_data_shreds: 32,
                num_code_shreds: 16,
                is_connected: true,
                completed: true,
            };
            storage.put_slot_meta(&meta).unwrap();
        });
    });

    group.bench_function("get_slot_meta", |b| {
        let (storage, _dir) = temp_storage();
        let meta = SlotMeta {
            slot: 500,
            parent_slot: 499,
            block_time: Some(500),
            num_data_shreds: 32,
            num_code_shreds: 16,
            is_connected: true,
            completed: true,
        };
        storage.put_slot_meta(&meta).unwrap();
        b.iter(|| {
            storage.get_slot_meta(500).unwrap();
        });
    });

    group.finish();
}

fn bench_snapshot(c: &mut Criterion) {
    let mut group = c.benchmark_group("snapshot");

    group.bench_function("put_snapshot", |b| {
        let (storage, _dir) = temp_storage();
        let mut slot = 0u64;
        b.iter(|| {
            slot += 1;
            let manifest = SnapshotManifest {
                slot,
                bank_hash: hash(format!("bank_{slot}").as_bytes()),
                account_count: 100_000,
                timestamp: slot as i64,
            };
            storage.put_snapshot(&manifest).unwrap();
        });
    });

    group.bench_function("get_latest_snapshot", |b| {
        let (storage, _dir) = temp_storage();
        for slot in (0..100).map(|i| i * 100) {
            let manifest = SnapshotManifest {
                slot,
                bank_hash: hash(format!("bank_{slot}").as_bytes()),
                account_count: 100_000,
                timestamp: slot as i64,
            };
            storage.put_snapshot(&manifest).unwrap();
        }
        b.iter(|| {
            storage.get_latest_snapshot().unwrap();
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_account_operations,
    bench_block_operations,
    bench_transaction_operations,
    bench_shred_operations,
    bench_consensus_state,
    bench_write_batch,
    bench_slot_meta,
    bench_snapshot,
);
criterion_main!(benches);
