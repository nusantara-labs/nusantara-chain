use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use nusantara_consensus::leader_schedule::LeaderScheduleGenerator;
use nusantara_core::epoch::EpochSchedule;
use nusantara_crypto::hash;

fn leader_schedule_compute(c: &mut Criterion) {
    let mut group = c.benchmark_group("leader_schedule_compute");

    for num_validators in [100, 1_000] {
        let es = EpochSchedule::new(10_000); // Smaller epoch for benchmark speed
        let lsg = LeaderScheduleGenerator::new(es);
        let seed = hash(b"bench_seed");
        let stakes: Vec<_> = (0..num_validators as u64)
            .map(|i| (hash(i.to_le_bytes().as_ref()), 1_000_000 + i * 100))
            .collect();

        group.bench_with_input(
            BenchmarkId::new("validators", num_validators),
            &num_validators,
            |b, _| {
                b.iter(|| {
                    lsg.compute_schedule(0, &stakes, &seed).unwrap();
                });
            },
        );
    }
    group.finish();
}

fn leader_schedule_lookup(c: &mut Criterion) {
    let es = EpochSchedule::new(10_000);
    let lsg = LeaderScheduleGenerator::new(es.clone());
    let seed = hash(b"bench_seed");
    let stakes: Vec<_> = (0..100u64)
        .map(|i| (hash(i.to_le_bytes().as_ref()), 1_000_000))
        .collect();

    let schedule = lsg.compute_schedule(0, &stakes, &seed).unwrap();

    c.bench_function("leader_schedule_lookup", |b| {
        b.iter(|| {
            for slot in 0..100 {
                schedule.get_leader(slot, &es);
            }
        });
    });
}

fn leader_schedule_validator_slots(c: &mut Criterion) {
    let es = EpochSchedule::new(10_000);
    let lsg = LeaderScheduleGenerator::new(es.clone());
    let seed = hash(b"bench_seed");
    let validator = hash(0u64.to_le_bytes().as_ref());
    let stakes: Vec<_> = (0..100u64)
        .map(|i| (hash(i.to_le_bytes().as_ref()), 1_000_000))
        .collect();

    let schedule = lsg.compute_schedule(0, &stakes, &seed).unwrap();

    c.bench_function("leader_schedule_validator_slots", |b| {
        b.iter(|| {
            schedule.get_slots_for_validator(&validator, &es);
        });
    });
}

criterion_group!(
    benches,
    leader_schedule_compute,
    leader_schedule_lookup,
    leader_schedule_validator_slots,
);
criterion_main!(benches);
