use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use nusantara_consensus::poh::{PohRecorder, verify_poh_entries};
use nusantara_crypto::hash;

fn poh_hash_iterations(c: &mut Criterion) {
    let mut group = c.benchmark_group("poh_hash_iterations");
    for count in [100, 1_000, 12_500] {
        group.bench_with_input(BenchmarkId::from_parameter(count), &count, |b, &count| {
            let init = hash(b"bench_genesis");
            b.iter(|| {
                let mut recorder = PohRecorder::new(init);
                recorder.hash_iterations(count);
            });
        });
    }
    group.finish();
}

fn poh_record_transaction(c: &mut Criterion) {
    let init = hash(b"bench_genesis");
    let tx_hash = hash(b"bench_tx");
    c.bench_function("poh_record_transaction", |b| {
        let mut recorder = PohRecorder::new(init);
        b.iter(|| {
            recorder.record(&tx_hash);
        });
    });
}

fn poh_tick_production(c: &mut Criterion) {
    let init = hash(b"bench_genesis");
    c.bench_function("poh_tick_production", |b| {
        b.iter(|| {
            let mut recorder = PohRecorder::new(init);
            recorder.tick();
        });
    });
}

fn poh_verify_entries(c: &mut Criterion) {
    let mut group = c.benchmark_group("poh_verify_entries");
    for count in [1, 10, 64] {
        let init = hash(b"bench_genesis");
        let mut recorder = PohRecorder::new(init);
        let entries: Vec<_> = (0..count)
            .map(|_| {
                let tick = recorder.tick();
                tick.entry
            })
            .collect();

        group.bench_with_input(
            BenchmarkId::new("entries", count),
            &entries,
            |b, entries| {
                b.iter(|| {
                    verify_poh_entries(&init, entries);
                });
            },
        );
    }
    group.finish();
}

criterion_group!(
    benches,
    poh_hash_iterations,
    poh_record_transaction,
    poh_tick_production,
    poh_verify_entries,
);
criterion_main!(benches);
