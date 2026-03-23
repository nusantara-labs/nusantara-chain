use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use nusantara_consensus::rewards::RewardsCalculator;
use nusantara_crypto::{Hash, hash};
use nusantara_stake_program::Delegation;
use nusantara_vote_program::{VoteInit, VoteState};

fn make_vote_state(node: Hash, commission: u8, epoch_credits: Vec<(u64, u64, u64)>) -> VoteState {
    let mut vs = VoteState::new(&VoteInit {
        node_pubkey: node,
        authorized_voter: node,
        authorized_withdrawer: node,
        commission,
    });
    vs.epoch_credits = epoch_credits;
    vs
}

fn rewards_calculate_epoch(c: &mut Criterion) {
    let mut group = c.benchmark_group("rewards_calculate_epoch");

    for num_stakers in [1_000, 10_000] {
        let num_validators = 100usize;
        let vote_states: Vec<(Hash, VoteState)> = (0..num_validators as u64)
            .map(|i| {
                let voter = hash(format!("voter_{i}").as_bytes());
                (voter, make_vote_state(voter, 10, vec![(1, 1000 + i, 0)]))
            })
            .collect();

        let delegations: Vec<(Hash, Delegation)> = (0..num_stakers as u64)
            .map(|i| {
                let voter_idx = i as usize % num_validators;
                let voter = vote_states[voter_idx].0;
                (
                    hash(format!("staker_{i}").as_bytes()),
                    Delegation {
                        voter_pubkey: voter,
                        stake: 1_000_000_000,
                        activation_epoch: 0,
                        deactivation_epoch: u64::MAX,
                        warmup_cooldown_rate_bps: 2500,
                    },
                )
            })
            .collect();

        group.bench_with_input(
            BenchmarkId::new("stakers", num_stakers),
            &num_stakers,
            |b, _| {
                b.iter(|| {
                    RewardsCalculator::calculate_epoch_rewards(
                        1,
                        100_000_000,
                        &vote_states,
                        &delegations,
                    )
                    .unwrap();
                });
            },
        );
    }
    group.finish();
}

fn rewards_point_calculation(c: &mut Criterion) {
    let num_delegations = 100_000usize;
    let voter = hash(b"voter");
    let vote_states = vec![(voter, make_vote_state(voter, 5, vec![(1, 1000, 0)]))];

    let delegations: Vec<(Hash, Delegation)> = (0..num_delegations as u64)
        .map(|i| {
            (
                hash(format!("s{i}").as_bytes()),
                Delegation {
                    voter_pubkey: voter,
                    stake: 1_000_000_000,
                    activation_epoch: 0,
                    deactivation_epoch: u64::MAX,
                    warmup_cooldown_rate_bps: 2500,
                },
            )
        })
        .collect();

    c.bench_function("rewards_100k_delegations", |b| {
        b.iter(|| {
            RewardsCalculator::calculate_epoch_rewards(
                1,
                1_000_000_000,
                &vote_states,
                &delegations,
            )
            .unwrap();
        });
    });
}

fn rewards_inflation_rate(c: &mut Criterion) {
    c.bench_function("rewards_inflation_rate_calc", |b| {
        b.iter(|| {
            for epoch in 0..100 {
                RewardsCalculator::inflation_rate_bps(epoch);
            }
        });
    });
}

criterion_group!(
    benches,
    rewards_calculate_epoch,
    rewards_point_calculation,
    rewards_inflation_rate,
);
criterion_main!(benches);
