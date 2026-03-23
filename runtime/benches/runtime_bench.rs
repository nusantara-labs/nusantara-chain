use criterion::{Criterion, criterion_group, criterion_main};
use nusantara_core::program::SYSTEM_PROGRAM_ID;
use nusantara_core::{Account, EpochSchedule, FeeCalculator, Message, Transaction};
use nusantara_crypto::{Keypair, hash};
use nusantara_rent_program::Rent;
use nusantara_runtime::account_loader::load_accounts;
use nusantara_runtime::compute_budget_parser::parse_compute_budget;
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
        RecentBlockhashes::default(),
    )
}

fn bench_single_transfer(c: &mut Criterion) {
    let dir = tempdir().unwrap();
    let storage = Storage::open(dir.path()).unwrap();
    let from_kp = Keypair::generate();
    let from = from_kp.address();
    let to = hash(b"bob");
    let fee_calc = FeeCalculator::default();
    let sysvars = test_sysvars();

    storage
        .put_account(&from, 0, &Account::new(u64::MAX / 2, *SYSTEM_PROGRAM_ID))
        .unwrap();

    let ix = nusantara_system_program::transfer(&from, &to, 100);
    let msg = Message::new(&[ix], &from).unwrap();
    let mut tx = Transaction::new(msg);
    tx.sign(&[&from_kp]);

    c.bench_function("single_transfer", |b| {
        b.iter(|| {
            execute_transaction(
                &tx,
                &storage,
                &sysvars,
                &fee_calc,
                1,
                &ProgramCache::new(16),
                None,
                false,
            );
        })
    });
}

fn bench_single_create_account(c: &mut Criterion) {
    let dir = tempdir().unwrap();
    let storage = Storage::open(dir.path()).unwrap();
    let from_kp = Keypair::generate();
    let from = from_kp.address();
    let fee_calc = FeeCalculator::default();
    let sysvars = test_sysvars();

    storage
        .put_account(&from, 0, &Account::new(u64::MAX / 2, *SYSTEM_PROGRAM_ID))
        .unwrap();

    let rent = Rent::default();
    let min = rent.minimum_balance(100);
    let new_acc_kp = Keypair::generate();
    let new_acc = new_acc_kp.address();
    let owner = hash(b"owner");
    let ix = nusantara_system_program::create_account(&from, &new_acc, min, 100, &owner);
    let msg = Message::new(&[ix], &from).unwrap();
    let mut tx = Transaction::new(msg);
    tx.sign(&[&from_kp, &new_acc_kp]);

    c.bench_function("single_create_account", |b| {
        b.iter(|| {
            execute_transaction(
                &tx,
                &storage,
                &sysvars,
                &fee_calc,
                1,
                &ProgramCache::new(16),
                None,
                false,
            );
        })
    });
}

fn bench_compute_budget_parsing(c: &mut Criterion) {
    let from = hash(b"payer");
    let to = hash(b"dest");

    let set_limit = nusantara_compute_budget_program::set_compute_unit_limit(500_000);
    let set_price = nusantara_compute_budget_program::set_compute_unit_price(1000);
    let transfer = nusantara_system_program::transfer(&from, &to, 100);
    let msg = Message::new(&[set_limit, set_price, transfer], &from).unwrap();

    c.bench_function("compute_budget_parsing", |b| {
        b.iter(|| {
            parse_compute_budget(&msg).unwrap();
        })
    });
}

fn bench_account_loading(c: &mut Criterion) {
    let dir = tempdir().unwrap();
    let storage = Storage::open(dir.path()).unwrap();

    let account_count = 10;
    let mut keys = Vec::new();
    for i in 0..account_count {
        let key = hash(format!("account_{i}").as_bytes());
        let mut acc = Account::new(1_000_000, *SYSTEM_PROGRAM_ID);
        acc.data = vec![0u8; 100];
        storage.put_account(&key, 0, &acc).unwrap();
        keys.push(key);
    }

    c.bench_function("account_loading_10", |b| {
        b.iter(|| {
            load_accounts(&storage, &keys, u32::MAX, None).unwrap();
        })
    });
}

criterion_group!(
    benches,
    bench_single_transfer,
    bench_single_create_account,
    bench_compute_budget_parsing,
    bench_account_loading,
);
criterion_main!(benches);
