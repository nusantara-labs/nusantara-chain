use std::collections::HashMap;

use nusantara_core::commitment::CommitmentLevel;
use nusantara_core::native_token::const_parse_u64;
use nusantara_crypto::Hash;
use tracing::instrument;

use crate::error::ConsensusError;

pub const OPTIMISTIC_CONFIRMATION_THRESHOLD: u64 =
    const_parse_u64(env!("NUSA_COMMITMENT_OPTIMISTIC_CONFIRMATION_THRESHOLD"));
pub const SUPERMAJORITY_THRESHOLD: u64 =
    const_parse_u64(env!("NUSA_COMMITMENT_SUPERMAJORITY_THRESHOLD"));
/// Maximum number of slots tracked in the commitment tracker.
/// Prevents unbounded memory growth as slots progress.
pub const MAX_TRACKED_SLOTS: usize = 1024;

#[derive(Clone, Debug)]
pub struct SlotCommitment {
    pub slot: u64,
    pub block_hash: Hash,
    pub total_stake_voted: u64,
    pub commitment: CommitmentLevel,
}

pub struct CommitmentTracker {
    slots: HashMap<u64, SlotCommitment>,
    total_active_stake: u64,
    highest_confirmed: u64,
    highest_finalized: u64,
}

impl CommitmentTracker {
    pub fn new(total_active_stake: u64) -> Self {
        Self {
            slots: HashMap::new(),
            total_active_stake,
            highest_confirmed: 0,
            highest_finalized: 0,
        }
    }

    pub fn update_total_stake(&mut self, total_active_stake: u64) {
        self.total_active_stake = total_active_stake;
    }

    /// Begin tracking a slot at Processed level.
    /// Prunes oldest entries when capacity exceeds `MAX_TRACKED_SLOTS`.
    #[instrument(skip(self), level = "debug")]
    pub fn track_slot(&mut self, slot: u64, block_hash: Hash) {
        self.slots.entry(slot).or_insert(SlotCommitment {
            slot,
            block_hash,
            total_stake_voted: 0,
            commitment: CommitmentLevel::Processed,
        });

        // Prune oldest entries when capacity exceeded
        if self.slots.len() > MAX_TRACKED_SLOTS {
            let mut slot_keys: Vec<u64> = self.slots.keys().copied().collect();
            slot_keys.sort_unstable();
            let to_remove = self.slots.len() - MAX_TRACKED_SLOTS;
            for &s in &slot_keys[..to_remove] {
                self.slots.remove(&s);
            }
        }
    }

    /// Record a vote for a slot, returning the new commitment level.
    #[instrument(skip(self), level = "debug")]
    pub fn record_vote(&mut self, slot: u64, block_hash: Hash, stake: u64) -> CommitmentLevel {
        let entry = self.slots.entry(slot).or_insert(SlotCommitment {
            slot,
            block_hash,
            total_stake_voted: 0,
            commitment: CommitmentLevel::Processed,
        });

        // Reject votes for a different block at the same slot — prevents
        // stake inflation from conflicting blocks.
        if entry.block_hash != block_hash {
            return entry.commitment;
        }

        entry.total_stake_voted = entry.total_stake_voted.saturating_add(stake);

        if self.total_active_stake > 0 {
            // Use u128 intermediate to prevent overflow when total_stake_voted > u64::MAX / 100
            let pct =
                (entry.total_stake_voted as u128 * 100 / self.total_active_stake as u128) as u64;
            if pct >= SUPERMAJORITY_THRESHOLD && entry.commitment != CommitmentLevel::Finalized {
                entry.commitment = CommitmentLevel::Confirmed;
                if slot > self.highest_confirmed {
                    self.highest_confirmed = slot;
                }
                metrics::gauge!("nusantara_commitment_highest_confirmed")
                    .set(self.highest_confirmed as f64);
            }
        }

        entry.commitment
    }

    /// Mark a slot as finalized (when Tower root advances past it).
    #[instrument(skip(self), level = "debug")]
    pub fn mark_finalized(&mut self, slot: u64) {
        if let Some(entry) = self.slots.get_mut(&slot) {
            entry.commitment = CommitmentLevel::Finalized;
            if slot > self.highest_finalized {
                self.highest_finalized = slot;
                metrics::gauge!("nusantara_commitment_highest_finalized")
                    .set(self.highest_finalized as f64);
            }
        }
    }

    pub fn highest_confirmed(&self) -> u64 {
        self.highest_confirmed
    }

    pub fn highest_finalized(&self) -> u64 {
        self.highest_finalized
    }

    pub fn get_commitment(&self, slot: u64) -> Result<CommitmentLevel, ConsensusError> {
        self.slots
            .get(&slot)
            .map(|s| s.commitment)
            .ok_or(ConsensusError::SlotNotTracked(slot))
    }

    pub fn get_slot_commitment(&self, slot: u64) -> Option<&SlotCommitment> {
        self.slots.get(&slot)
    }

    /// Prune entries below the given slot.
    #[instrument(skip(self), level = "debug")]
    pub fn prune_below(&mut self, slot: u64) {
        self.slots.retain(|&s, _| s >= slot);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_values() {
        assert_eq!(OPTIMISTIC_CONFIRMATION_THRESHOLD, 66);
        assert_eq!(SUPERMAJORITY_THRESHOLD, 66);
    }

    #[test]
    fn track_and_vote() {
        let total_stake = 1000;
        let mut tracker = CommitmentTracker::new(total_stake);
        let block_hash = nusantara_crypto::hash(b"block1");

        tracker.track_slot(1, block_hash);
        let level = tracker.get_commitment(1).unwrap();
        assert_eq!(level, CommitmentLevel::Processed);

        // Vote with 50% stake -> still Processed
        let level = tracker.record_vote(1, block_hash, 500);
        assert_eq!(level, CommitmentLevel::Processed);

        // Vote with 17% more -> 67% -> Confirmed
        let level = tracker.record_vote(1, block_hash, 170);
        assert_eq!(level, CommitmentLevel::Confirmed);
        assert_eq!(tracker.highest_confirmed(), 1);
    }

    #[test]
    fn finalize_slot() {
        let mut tracker = CommitmentTracker::new(1000);
        let block_hash = nusantara_crypto::hash(b"block");

        tracker.track_slot(5, block_hash);
        tracker.record_vote(5, block_hash, 700);
        tracker.mark_finalized(5);

        assert_eq!(
            tracker.get_commitment(5).unwrap(),
            CommitmentLevel::Finalized
        );
        assert_eq!(tracker.highest_finalized(), 5);
    }

    #[test]
    fn prune_below() {
        let mut tracker = CommitmentTracker::new(1000);
        let h = nusantara_crypto::hash(b"h");
        for slot in 1..=10 {
            tracker.track_slot(slot, h);
        }
        tracker.prune_below(5);
        assert!(tracker.get_commitment(4).is_err());
        assert!(tracker.get_commitment(5).is_ok());
    }

    #[test]
    fn untracked_slot_error() {
        let tracker = CommitmentTracker::new(1000);
        assert!(tracker.get_commitment(99).is_err());
    }

    #[test]
    fn large_stake_commitment_no_overflow() {
        // total_stake_voted * 100 would overflow u64 for large values
        let total_stake = u64::MAX / 10;
        let mut tracker = CommitmentTracker::new(total_stake);
        let h = nusantara_crypto::hash(b"block");

        tracker.track_slot(1, h);
        // Vote with 70% of total_stake — use u128 to compute the value without overflow
        let vote_stake = (total_stake as u128 * 70 / 100) as u64;
        let level = tracker.record_vote(1, h, vote_stake);
        // 70% >= 66% threshold -> Confirmed
        assert_eq!(level, CommitmentLevel::Confirmed);
    }

    #[test]
    fn max_tracked_slots_pruning() {
        let mut tracker = CommitmentTracker::new(1000);
        let h = nusantara_crypto::hash(b"h");

        // Fill beyond MAX_TRACKED_SLOTS
        for slot in 1..=(MAX_TRACKED_SLOTS as u64 + 100) {
            tracker.track_slot(slot, h);
        }

        // Should be pruned to MAX_TRACKED_SLOTS
        assert_eq!(tracker.slots.len(), MAX_TRACKED_SLOTS);

        // Oldest slots should be gone
        assert!(tracker.get_commitment(1).is_err());
        assert!(tracker.get_commitment(100).is_err());

        // Newest slots should still be present
        let newest = MAX_TRACKED_SLOTS as u64 + 100;
        assert!(tracker.get_commitment(newest).is_ok());
    }
}
