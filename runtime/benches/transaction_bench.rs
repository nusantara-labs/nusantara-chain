use criterion::{Criterion, criterion_group, criterion_main};
use nusantara_core::program::SYSTEM_PROGRAM_ID;
use nusantara_core::{Account, EpochSchedule, FeeCalculator, Message, Transaction};
use nusantara_crypto::{Keypair, hash};
use nusantara_rent_program::Rent;
use nusantara_runtime::{ProgramCache, SysvarCache, execute_slot};
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
        RecentBlockhashes::default(),
    )
}

fn make_transfer_batch(count: usize) -> (Storage, tempfile::TempDir, Vec<Transaction>) {
    let dir = tempdir().unwrap();
    let storage = Storage::open(dir.path()).unwrap();

    let mut txs = Vec::with_capacity(count);
    for i in 0..count {
        let from_kp = Keypair::generate();
        let from = from_kp.address();
        let to = hash(format!("receiver_{i}").as_bytes());

        storage
            .put_account(&from, 0, &Account::new(1_000_000, *SYSTEM_PROGRAM_ID))
            .unwrap();

        let ix = nusantara_system_program::transfer(&from, &to, 100);
        let msg = Message::new(&[ix], &from).unwrap();
        let mut tx = Transaction::new(msg);
        tx.sign(&[&from_kp]);
        txs.push(tx);
    }

    (storage, dir, txs)
}

fn bench_batch_transfers_10(c: &mut Criterion) {
    let (storage, _dir, txs) = make_transfer_batch(10);
    let sysvars = test_sysvars();
    let fee_calc = FeeCalculator::default();

    c.bench_function("batch_transfers_10", |b| {
        b.iter(|| {
            execute_slot(
                1,
                &txs,
                &storage,
                &sysvars,
                &fee_calc,
                &ProgramCache::new(16),
            )
            .unwrap();
        })
    });
}

fn bench_batch_transfers_100(c: &mut Criterion) {
    let (storage, _dir, txs) = make_transfer_batch(100);
    let sysvars = test_sysvars();
    let fee_calc = FeeCalculator::default();

    c.bench_function("batch_transfers_100", |b| {
        b.iter(|| {
            execute_slot(
                1,
                &txs,
                &storage,
                &sysvars,
                &fee_calc,
                &ProgramCache::new(16),
            )
            .unwrap();
        })
    });
}

fn bench_batch_mixed_100(c: &mut Criterion) {
    let dir = tempdir().unwrap();
    let storage = Storage::open(dir.path()).unwrap();
    let sysvars = test_sysvars();
    let fee_calc = FeeCalculator::default();

    let rent = Rent::default();
    let mut txs = Vec::with_capacity(100);

    for i in 0..100 {
        let from_kp = Keypair::generate();
        let from = from_kp.address();
        storage
            .put_account(&from, 0, &Account::new(10_000_000, *SYSTEM_PROGRAM_ID))
            .unwrap();

        if i % 2 == 0 {
            // Transfer
            let to = hash(format!("dest_{i}").as_bytes());
            let ix = nusantara_system_program::transfer(&from, &to, 100);
            let msg = Message::new(&[ix], &from).unwrap();
            let mut tx = Transaction::new(msg);
            tx.sign(&[&from_kp]);
            txs.push(tx);
        } else {
            // Create account
            let new_acc_kp = Keypair::generate();
            let new_acc = new_acc_kp.address();
            let owner = hash(b"owner");
            let min = rent.minimum_balance(100);
            let ix = nusantara_system_program::create_account(&from, &new_acc, min, 100, &owner);
            let msg = Message::new(&[ix], &from).unwrap();
            let mut tx = Transaction::new(msg);
            tx.sign(&[&from_kp, &new_acc_kp]);
            txs.push(tx);
        }
    }

    c.bench_function("batch_mixed_100", |b| {
        b.iter(|| {
            execute_slot(
                1,
                &txs,
                &storage,
                &sysvars,
                &fee_calc,
                &ProgramCache::new(16),
            )
            .unwrap();
        })
    });
}

criterion_group!(
    benches,
    bench_batch_transfers_10,
    bench_batch_transfers_100,
    bench_batch_mixed_100,
);
criterion_main!(benches);
