use std::collections::HashSet;

use nusantara_core::EpochSchedule;
use nusantara_crypto::Hash;
use nusantara_rent_program::Rent;
use nusantara_sysvar_program::{Clock, RecentBlockhashes, SlotHashes, StakeHistory};

/// Builder for [`SysvarCache`] with fluent setters.
///
/// All fields default to their `Default` values. Override only what you need.
///
/// ```ignore
/// let cache = SysvarCacheBuilder::new()
///     .clock(Clock { slot: 100, epoch: 5, ..Clock::default() })
///     .build();
/// ```
#[derive(Default)]
pub struct SysvarCacheBuilder {
    clock: Clock,
    rent: Rent,
    epoch_schedule: EpochSchedule,
    slot_hashes: SlotHashes,
    stake_history: StakeHistory,
    recent_blockhashes: RecentBlockhashes,
}

impl SysvarCacheBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn clock(mut self, clock: Clock) -> Self {
        self.clock = clock;
        self
    }

    pub fn rent(mut self, rent: Rent) -> Self {
        self.rent = rent;
        self
    }

    pub fn epoch_schedule(mut self, epoch_schedule: EpochSchedule) -> Self {
        self.epoch_schedule = epoch_schedule;
        self
    }

    pub fn slot_hashes(mut self, slot_hashes: SlotHashes) -> Self {
        self.slot_hashes = slot_hashes;
        self
    }

    pub fn stake_history(mut self, stake_history: StakeHistory) -> Self {
        self.stake_history = stake_history;
        self
    }

    pub fn recent_blockhashes(mut self, recent_blockhashes: RecentBlockhashes) -> Self {
        self.recent_blockhashes = recent_blockhashes;
        self
    }

    pub fn build(self) -> SysvarCache {
        let recent_blockhash_set: HashSet<Hash> =
            self.recent_blockhashes.0.iter().copied().collect();
        SysvarCache {
            clock: self.clock,
            rent: self.rent,
            epoch_schedule: self.epoch_schedule,
            slot_hashes: self.slot_hashes,
            stake_history: self.stake_history,
            recent_blockhashes: self.recent_blockhashes,
            recent_blockhash_set,
        }
    }
}

pub struct SysvarCache {
    clock: Clock,
    rent: Rent,
    epoch_schedule: EpochSchedule,
    slot_hashes: SlotHashes,
    stake_history: StakeHistory,
    recent_blockhashes: RecentBlockhashes,
    recent_blockhash_set: HashSet<Hash>,
}

impl SysvarCache {
    pub fn new(
        clock: Clock,
        rent: Rent,
        epoch_schedule: EpochSchedule,
        slot_hashes: SlotHashes,
        stake_history: StakeHistory,
        recent_blockhashes: RecentBlockhashes,
    ) -> Self {
        let recent_blockhash_set: HashSet<Hash> = recent_blockhashes.0.iter().copied().collect();
        Self {
            clock,
            rent,
            epoch_schedule,
            slot_hashes,
            stake_history,
            recent_blockhashes,
            recent_blockhash_set,
        }
    }

    /// O(1) blockhash lookup using the pre-built HashSet.
    pub fn contains_blockhash(&self, hash: &Hash) -> bool {
        self.recent_blockhash_set.contains(hash)
    }

    pub fn clock(&self) -> &Clock {
        &self.clock
    }

    pub fn rent(&self) -> &Rent {
        &self.rent
    }

    pub fn epoch_schedule(&self) -> &EpochSchedule {
        &self.epoch_schedule
    }

    pub fn slot_hashes(&self) -> &SlotHashes {
        &self.slot_hashes
    }

    pub fn stake_history(&self) -> &StakeHistory {
        &self.stake_history
    }

    pub fn recent_blockhashes(&self) -> &RecentBlockhashes {
        &self.recent_blockhashes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nusantara_crypto::hash;

    fn test_cache() -> SysvarCache {
        SysvarCache::new(
            Clock::default(),
            Rent::default(),
            EpochSchedule::default(),
            SlotHashes::default(),
            StakeHistory::default(),
            RecentBlockhashes::new(vec![hash(b"blockhash1")]),
        )
    }

    #[test]
    fn construction() {
        let cache = test_cache();
        assert_eq!(cache.clock().slot, 0);
        assert_eq!(cache.rent().lamports_per_byte_year, 3480);
    }

    #[test]
    fn rent_minimum() {
        let cache = test_cache();
        let min = cache.rent().minimum_balance(0);
        assert_eq!(min, 890_880);
    }

    #[test]
    fn recent_blockhashes_contains() {
        let cache = test_cache();
        let h = hash(b"blockhash1");
        assert!(cache.recent_blockhashes().contains(&h));
        assert!(!cache.recent_blockhashes().contains(&hash(b"other")));
    }

    #[test]
    fn contains_blockhash_hashset() {
        let cache = test_cache();
        let h = hash(b"blockhash1");
        assert!(cache.contains_blockhash(&h));
        assert!(!cache.contains_blockhash(&hash(b"other")));
    }
}
