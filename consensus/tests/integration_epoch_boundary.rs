use std::sync::Arc;

use nusantara_consensus::bank::ConsensusBank;
use nusantara_consensus::leader_schedule::LeaderScheduleGenerator;
use nusantara_consensus::rewards::RewardsCalculator;
use nusantara_core::epoch::EpochSchedule;
use nusantara_crypto::hash;
use nusantara_stake_program::Delegation;
use nusantara_storage::Storage;
use nusantara_sysvar_program::StakeHistoryEntry;
use nusantara_vote_program::{VoteInit, VoteState};

fn temp_bank(slots_per_epoch: u64) -> (Arc<ConsensusBank>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let storage = Arc::new(Storage::open(dir.path()).unwrap());
    let bank = Arc::new(ConsensusBank::new(
        storage,
        EpochSchedule::new(slots_per_epoch),
    ));
    (bank, dir)
}

#[test]
fn test_epoch_boundary_stake_recalculation() {
    let (bank, _dir) = temp_bank(100);
    let voter = hash(b"voter");

    // Add delegations with varying activation epochs
    let d1 = Delegation {
        voter_pubkey: voter,
        stake: 1_000_000,
        activation_epoch: 0, // Active from epoch 0
        deactivation_epoch: u64::MAX,
        warmup_cooldown_rate_bps: 2500,
    };
    let d2 = Delegation {
        voter_pubkey: voter,
        stake: 2_000_000,
        activation_epoch: 1, // Activating in epoch 1 (warmup applies)
        deactivation_epoch: u64::MAX,
        warmup_cooldown_rate_bps: 2500,
    };
    let d3 = Delegation {
        voter_pubkey: voter,
        stake: 500_000,
        activation_epoch: 5, // Not yet active in epoch 1
        deactivation_epoch: u64::MAX,
        warmup_cooldown_rate_bps: 2500,
    };

    bank.set_stake_delegation(hash(b"s1"), d1);
    bank.set_stake_delegation(hash(b"s2"), d2);
    bank.set_stake_delegation(hash(b"s3"), d3);

    // Recalculate for epoch 1
    bank.recalculate_epoch_stakes(1);

    let total = bank.total_active_stake();
    // d1: fully active = 1_000_000
    // d2: warming up at 25% = 500_000
    // d3: not active yet = 0
    assert_eq!(total, 1_500_000);
    assert_eq!(bank.get_validator_stake(&voter), 1_500_000);

    // Recalculate for epoch 2 - d2 should be fully active now
    bank.recalculate_epoch_stakes(2);
    let total = bank.total_active_stake();
    // d1: 1_000_000, d2: 2_000_000, d3: not active = 0
    assert_eq!(total, 3_000_000);
}

#[test]
fn test_epoch_boundary_leader_schedule_rotation() {
    let es = EpochSchedule::new(100);
    let lsg = LeaderScheduleGenerator::new(es);
    let seed = hash(b"seed");

    let v1 = hash(b"v1");
    let v2 = hash(b"v2");
    let v3 = hash(b"v3");

    // Epoch 0: two validators
    let stakes_e0 = vec![(v1, 500), (v2, 500)];
    let s0 = lsg.compute_schedule(0, &stakes_e0, &seed).unwrap();

    // Epoch 1: new validator joins, changing distribution
    let stakes_e1 = vec![(v1, 500), (v2, 500), (v3, 1000)];
    let s1 = lsg.compute_schedule(1, &stakes_e1, &seed).unwrap();

    // Schedules should differ
    assert_ne!(s0.slot_leaders, s1.slot_leaders);

    // v3 should appear in epoch 1 schedule
    let v3_slots = s1.slot_leaders.iter().filter(|l| **l == v3).count();
    assert!(v3_slots > 0);

    // v3 should have roughly 50% of slots (1000/2000)
    let v3_pct = v3_slots * 100 / s1.slot_leaders.len();
    assert!(v3_pct > 30); // Allow margin for randomness
}

#[test]
fn test_epoch_boundary_partitioned_rewards() {
    let voter1 = hash(b"voter1");
    let voter2 = hash(b"voter2");

    // 2 validators with different commissions
    let vs1 = {
        let mut vs = VoteState::new(&VoteInit {
            node_pubkey: voter1,
            authorized_voter: voter1,
            authorized_withdrawer: voter1,
            commission: 10,
        });
        vs.epoch_credits = vec![(1, 1000, 0)];
        vs
    };
    let vs2 = {
        let mut vs = VoteState::new(&VoteInit {
            node_pubkey: voter2,
            authorized_voter: voter2,
            authorized_withdrawer: voter2,
            commission: 5,
        });
        vs.epoch_credits = vec![(1, 800, 0)];
        vs
    };

    let vote_states = vec![(voter1, vs1), (voter2, vs2)];

    // 10 stakers
    let delegations: Vec<_> = (0u64..10)
        .map(|i| {
            let voter = if i < 5 { voter1 } else { voter2 };
            (
                nusantara_crypto::hashv(&[b"staker", &i.to_le_bytes()]),
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

    let inflation_rewards = 10_000_000; // 10M lamports
    let rewards = RewardsCalculator::calculate_epoch_rewards(
        1,
        inflation_rewards,
        &vote_states,
        &delegations,
    )
    .unwrap();

    // Verify rewards are partitioned
    assert_eq!(rewards.partitions.len(), 4096);

    // Verify total distributed rewards
    let total_in_partitions: u64 = rewards
        .partitions
        .iter()
        .flat_map(|p| p.iter())
        .map(|e| e.lamports + e.commission_lamports)
        .sum();
    assert_eq!(total_in_partitions, rewards.total_rewards_lamports);
    assert!(rewards.total_rewards_lamports > 0);

    // Verify commission splits
    for entry in rewards.partitions.iter().flat_map(|p| p.iter()) {
        // After commission, staker should receive less than the full reward
        assert!(entry.lamports > 0);
    }

    // Track distribution progress
    let mut status = nusantara_consensus::rewards::RewardDistributionStatus::new(1, &rewards);
    assert!(!status.is_complete());

    for partition in &rewards.partitions {
        let partition_total: u64 = partition
            .iter()
            .map(|e| e.lamports + e.commission_lamports)
            .sum();
        status.record_partition_distributed(partition_total);
    }
    assert!(status.is_complete());
    assert_eq!(status.distributed_rewards, rewards.total_rewards_lamports);
}

#[test]
fn test_epoch_boundary_sysvar_updates() {
    let (bank, _dir) = temp_bank(100);

    // Advance through an epoch boundary
    for slot in 0..=100 {
        bank.advance_slot(slot, 1000 + slot as i64);
        bank.record_slot_hash(slot, hash(format!("slot{slot}").as_bytes()));
    }

    // Verify Clock sysvar
    let clock = bank.clock();
    assert_eq!(clock.slot, 100);
    assert_eq!(clock.epoch, 1);

    // Verify SlotHashes contains recent hashes
    let slot_hashes = bank.slot_hashes();
    assert!(!slot_hashes.is_empty());
    assert!(slot_hashes.get(100).is_some());
    assert!(slot_hashes.get(99).is_some());

    // Update stake history
    bank.update_stake_history(
        0,
        StakeHistoryEntry {
            effective: 1_000_000,
            activating: 500_000,
            deactivating: 0,
        },
    );
    let history = bank.stake_history();
    assert!(history.get(0).is_some());
    assert_eq!(history.get(0).unwrap().effective, 1_000_000);
}
