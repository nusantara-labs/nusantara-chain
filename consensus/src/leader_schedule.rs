use borsh::{BorshDeserialize, BorshSerialize};
use nusantara_core::epoch::EpochSchedule;
use nusantara_core::native_token::const_parse_u64;
use nusantara_crypto::{Hash, hashv};
use tracing::instrument;

use crate::error::ConsensusError;

pub const NUM_CONSECUTIVE_LEADER_SLOTS: u64 =
    const_parse_u64(env!("NUSA_LEADER_SCHEDULE_NUM_CONSECUTIVE_LEADER_SLOTS"));

#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct LeaderSchedule {
    pub epoch: u64,
    pub slot_leaders: Vec<Hash>,
}

impl LeaderSchedule {
    pub fn get_leader(&self, slot: u64, epoch_schedule: &EpochSchedule) -> Option<&Hash> {
        let first_slot = epoch_schedule.get_first_slot_in_epoch(self.epoch);
        if slot < first_slot {
            return None;
        }
        let index = (slot - first_slot) as usize;
        self.slot_leaders.get(index)
    }

    pub fn get_slots_for_validator(
        &self,
        validator: &Hash,
        epoch_schedule: &EpochSchedule,
    ) -> Vec<u64> {
        let first_slot = epoch_schedule.get_first_slot_in_epoch(self.epoch);
        self.slot_leaders
            .iter()
            .enumerate()
            .filter(|(_, leader)| *leader == validator)
            .map(|(i, _)| first_slot + i as u64)
            .collect()
    }
}

#[derive(Clone)]
pub struct LeaderScheduleGenerator {
    pub epoch_schedule: EpochSchedule,
}

impl LeaderScheduleGenerator {
    pub fn new(epoch_schedule: EpochSchedule) -> Self {
        Self { epoch_schedule }
    }

    /// Compute a deterministic leader schedule for the given epoch.
    /// Stakes is a list of (validator_identity_hash, stake_amount).
    /// The schedule is seeded from hashv(&[epoch_seed, &epoch.to_le_bytes()]).
    #[instrument(skip(self, stakes, epoch_seed), level = "info")]
    pub fn compute_schedule(
        &self,
        epoch: u64,
        stakes: &[(Hash, u64)],
        epoch_seed: &Hash,
    ) -> Result<LeaderSchedule, ConsensusError> {
        // Filter to validators with non-zero stake, sorted by identity
        // for determinism (input may come from DashMap with random iteration order)
        let mut active_stakes: Vec<(Hash, u64)> =
            stakes.iter().filter(|(_, s)| *s > 0).cloned().collect();
        active_stakes.sort_by_key(|a| a.0);

        if active_stakes.is_empty() {
            return Err(ConsensusError::NoValidatorsWithStake(epoch));
        }

        // B4: use checked_add to detect stake overflow.
        let total_stake: u64 = active_stakes
            .iter()
            .try_fold(0u64, |acc, (_, s)| acc.checked_add(*s))
            .ok_or(ConsensusError::StakeOverflow(epoch))?;

        let slots_in_epoch = self.epoch_schedule.slots_per_epoch;

        // Number of leader assignments (each gets NUM_CONSECUTIVE_LEADER_SLOTS)
        let num_assignments = slots_in_epoch / NUM_CONSECUTIVE_LEADER_SLOTS;

        // Deterministic seed
        let seed = hashv(&[epoch_seed.as_bytes(), &epoch.to_le_bytes()]);

        let mut slot_leaders = Vec::with_capacity(slots_in_epoch as usize);
        let mut rng_state = seed;

        for assignment in 0..num_assignments {
            // Generate a deterministic random value for this assignment
            rng_state = hashv(&[rng_state.as_bytes(), &assignment.to_le_bytes()]);
            // B18: extracted into pick_validator helper.
            let selected = Self::pick_validator(&mut rng_state, &active_stakes, total_stake);
            for _ in 0..NUM_CONSECUTIVE_LEADER_SLOTS {
                slot_leaders.push(selected);
            }
        }

        // Handle remainder slots (if slots_per_epoch is not divisible)
        let remainder = slots_in_epoch - (num_assignments * NUM_CONSECUTIVE_LEADER_SLOTS);
        if remainder > 0 {
            rng_state = hashv(&[rng_state.as_bytes(), &num_assignments.to_le_bytes()]);
            // B18: reuse pick_validator helper.
            let selected = Self::pick_validator(&mut rng_state, &active_stakes, total_stake);
            for _ in 0..remainder {
                slot_leaders.push(selected);
            }
        }

        metrics::counter!("nusantara_leader_schedule_computed_total").increment(1);

        Ok(LeaderSchedule {
            epoch,
            slot_leaders,
        })
    }

    /// B18: Deterministic stake-weighted validator selection with rejection sampling.
    ///
    /// Advances `rng_state` in-place. `total_stake` must be > 0 (guaranteed by caller).
    /// Uses u128 for `cumulative` to avoid overflow when summing large stakes (B4).
    fn pick_validator(
        rng_state: &mut Hash,
        active_stakes: &[(Hash, u64)],
        total_stake: u64,
    ) -> Hash {
        let mut rng_bytes = rng_state.as_bytes();
        let mut rng_val = u64::from_le_bytes([
            rng_bytes[0], rng_bytes[1], rng_bytes[2], rng_bytes[3],
            rng_bytes[4], rng_bytes[5], rng_bytes[6], rng_bytes[7],
        ]);

        // Rejection sampling to eliminate modulo bias.
        let max_unbiased = (u64::MAX / total_stake) * total_stake;
        while rng_val >= max_unbiased {
            *rng_state = hashv(&[rng_state.as_bytes(), &rng_val.to_le_bytes()]);
            rng_bytes = rng_state.as_bytes();
            rng_val = u64::from_le_bytes([
                rng_bytes[0], rng_bytes[1], rng_bytes[2], rng_bytes[3],
                rng_bytes[4], rng_bytes[5], rng_bytes[6], rng_bytes[7],
            ]);
        }

        // Stake-weighted selection; use u128 for cumulative to avoid overflow (B4).
        let target = rng_val % total_stake;
        let mut cumulative: u128 = 0;
        let mut selected = active_stakes[0].0;

        for (validator, stake) in active_stakes {
            cumulative += *stake as u128;
            if cumulative > target as u128 {
                selected = *validator;
                break;
            }
        }

        selected
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nusantara_crypto::hash;

    #[test]
    fn config_values() {
        assert_eq!(NUM_CONSECUTIVE_LEADER_SLOTS, 4);
    }

    #[test]
    fn deterministic_schedule() {
        let es = EpochSchedule::new(100);
        let lsg = LeaderScheduleGenerator::new(es.clone());
        let seed = hash(b"epoch_seed");

        let v1 = hash(b"validator1");
        let v2 = hash(b"validator2");
        let stakes = vec![(v1, 500), (v2, 500)];

        let s1 = lsg.compute_schedule(0, &stakes, &seed).unwrap();
        let s2 = lsg.compute_schedule(0, &stakes, &seed).unwrap();

        assert_eq!(s1, s2);
    }

    #[test]
    fn schedule_covers_all_slots() {
        let es = EpochSchedule::new(100);
        let lsg = LeaderScheduleGenerator::new(es);
        let seed = hash(b"seed");
        let stakes = vec![(hash(b"v1"), 1000)];

        let schedule = lsg.compute_schedule(0, &stakes, &seed).unwrap();
        assert_eq!(schedule.slot_leaders.len(), 100);
    }

    #[test]
    fn consecutive_leader_slots() {
        let es = EpochSchedule::new(100);
        let lsg = LeaderScheduleGenerator::new(es);
        let seed = hash(b"seed");
        let stakes = vec![(hash(b"v1"), 1000)];

        let schedule = lsg.compute_schedule(0, &stakes, &seed).unwrap();

        // With one validator, all slots should be theirs
        let v1 = hash(b"v1");
        assert!(schedule.slot_leaders.iter().all(|l| *l == v1));
    }

    #[test]
    fn stake_weighted_distribution() {
        let es = EpochSchedule::new(10000);
        let lsg = LeaderScheduleGenerator::new(es);
        let seed = hash(b"weighted_seed");

        let v1 = hash(b"big_validator");
        let v2 = hash(b"small_validator");
        let stakes = vec![(v1, 9000), (v2, 1000)];

        let schedule = lsg.compute_schedule(0, &stakes, &seed).unwrap();
        let v1_count = schedule.slot_leaders.iter().filter(|l| **l == v1).count();
        let v2_count = schedule.slot_leaders.iter().filter(|l| **l == v2).count();

        assert!(v1_count > v2_count);
        let v1_pct = v1_count * 100 / schedule.slot_leaders.len();
        assert!(v1_pct > 70);
    }

    #[test]
    fn get_leader_for_slot() {
        let es = EpochSchedule::new(100);
        let lsg = LeaderScheduleGenerator::new(es.clone());
        let seed = hash(b"seed");
        let v1 = hash(b"v1");
        let stakes = vec![(v1, 1000)];

        let schedule = lsg.compute_schedule(0, &stakes, &seed).unwrap();
        assert_eq!(schedule.get_leader(0, &es), Some(&v1));
        assert_eq!(schedule.get_leader(99, &es), Some(&v1));
        assert_eq!(schedule.get_leader(100, &es), None);
    }

    #[test]
    fn no_validators_error() {
        let es = EpochSchedule::new(100);
        let lsg = LeaderScheduleGenerator::new(es);
        let seed = hash(b"seed");
        let stakes: Vec<(Hash, u64)> = vec![];

        let result = lsg.compute_schedule(0, &stakes, &seed);
        assert!(result.is_err());
    }

    #[test]
    fn get_slots_for_validator() {
        let es = EpochSchedule::new(20);
        let lsg = LeaderScheduleGenerator::new(es.clone());
        let seed = hash(b"seed");
        let v1 = hash(b"v1");
        let stakes = vec![(v1, 1000)];

        let schedule = lsg.compute_schedule(0, &stakes, &seed).unwrap();
        let slots = schedule.get_slots_for_validator(&v1, &es);
        assert_eq!(slots.len(), 20);
    }

    #[test]
    fn different_epochs_different_schedules() {
        let es = EpochSchedule::new(100);
        let lsg = LeaderScheduleGenerator::new(es);
        let seed = hash(b"seed");
        let v1 = hash(b"v1");
        let v2 = hash(b"v2");
        let stakes = vec![(v1, 500), (v2, 500)];

        let s0 = lsg.compute_schedule(0, &stakes, &seed).unwrap();
        let s1 = lsg.compute_schedule(1, &stakes, &seed).unwrap();

        assert_ne!(s0.slot_leaders, s1.slot_leaders);
    }
}
