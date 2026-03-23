use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use nusantara_consensus::tower::{MAX_LOCKOUT_HISTORY, Tower};
use nusantara_crypto::hash;
use nusantara_vote_program::{Vote, VoteInit, VoteState};

fn make_tower() -> Tower {
    let init = VoteInit {
        node_pubkey: hash(b"node"),
        authorized_voter: hash(b"voter"),
        authorized_withdrawer: hash(b"wd"),
        commission: 10,
    };
    Tower::new(VoteState::new(&init))
}

fn make_vote(slot: u64) -> Vote {
    Vote {
        slots: vec![slot],
        hash: hash(slot.to_le_bytes().as_ref()),
        timestamp: None,
    }
}

fn tower_process_vote(c: &mut Criterion) {
    let mut group = c.benchmark_group("tower_process_vote");

    for depth in [0, 15, 30] {
        group.bench_with_input(BenchmarkId::new("depth", depth), &depth, |b, &depth| {
            b.iter(|| {
                let mut tower = make_tower();
                // Pre-fill tower to desired depth
                for slot in 1..=depth {
                    tower.process_vote(&make_vote(slot)).unwrap();
                }
                // Benchmark the next vote
                tower.process_vote(&make_vote(depth + 1)).unwrap();
            });
        });
    }
    group.finish();
}

fn tower_check_lockout(c: &mut Criterion) {
    let mut tower = make_tower();
    for slot in 1..=30 {
        tower.process_vote(&make_vote(slot)).unwrap();
    }

    c.bench_function("tower_check_lockout_full", |b| {
        b.iter(|| {
            tower.check_vote_lockout(31).unwrap();
        });
    });
}

fn tower_switch_threshold(c: &mut Criterion) {
    let tower = make_tower();
    let stakes: Vec<_> = (0..1000u64)
        .map(|i| (hash(i.to_le_bytes().as_ref()), 1000))
        .collect();

    c.bench_function("tower_switch_threshold_1000_validators", |b| {
        b.iter(|| {
            tower.check_switch_threshold(100, &stakes, 1_000_000);
        });
    });
}

fn tower_root_advancement(c: &mut Criterion) {
    c.bench_function("tower_root_advancement_cycle", |b| {
        b.iter(|| {
            let mut tower = make_tower();
            for slot in 1..=MAX_LOCKOUT_HISTORY {
                tower.process_vote(&make_vote(slot)).unwrap();
            }
        });
    });
}

criterion_group!(
    benches,
    tower_process_vote,
    tower_check_lockout,
    tower_switch_threshold,
    tower_root_advancement,
);
criterion_main!(benches);
