use borsh::{BorshDeserialize, BorshSerialize};

use crate::native_token::const_parse_u64;

pub const DEFAULT_SLOTS_PER_EPOCH: u64 = const_parse_u64(env!("NUSA_TIMING_SLOTS_PER_EPOCH"));
pub const DEFAULT_SLOT_DURATION_MS: u64 = const_parse_u64(env!("NUSA_TIMING_SLOT_DURATION_MS"));
pub const DEFAULT_LEADER_SCHEDULE_SLOT_OFFSET: u64 =
    const_parse_u64(env!("NUSA_TIMING_LEADER_SCHEDULE_SLOT_OFFSET"));

#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct EpochSchedule {
    pub slots_per_epoch: u64,
    pub leader_schedule_slot_offset: u64,
    pub warmup: bool,
    pub first_normal_epoch: u64,
    pub first_normal_slot: u64,
}

impl EpochSchedule {
    pub fn new(slots_per_epoch: u64) -> Self {
        Self {
            slots_per_epoch,
            leader_schedule_slot_offset: slots_per_epoch,
            warmup: false,
            first_normal_epoch: 0,
            first_normal_slot: 0,
        }
    }

    pub fn get_epoch(&self, slot: u64) -> u64 {
        self.get_epoch_and_slot_index(slot).0
    }

    pub fn get_epoch_and_slot_index(&self, slot: u64) -> (u64, u64) {
        if slot < self.first_normal_slot {
            // Warmup branch: epochs double in size each epoch.
            // `checked_shl` guards against overflow when epoch >= 64 (theoretically
            // impossible with u64 slot space, but prevents UB-by-panic on malformed state).
            let epoch = slot.checked_ilog2().unwrap_or(0) as u64;
            let epoch_len = 1u64.checked_shl(epoch as u32).unwrap_or(u64::MAX);
            let epoch_start = epoch_len.saturating_sub(1);
            let slot_index = slot.saturating_sub(epoch_start);
            (epoch, slot_index)
        } else {
            let normal_slot = slot - self.first_normal_slot;
            let epoch = self.first_normal_epoch + normal_slot / self.slots_per_epoch;
            let slot_index = normal_slot % self.slots_per_epoch;
            (epoch, slot_index)
        }
    }

    pub fn get_first_slot_in_epoch(&self, epoch: u64) -> u64 {
        if epoch <= self.first_normal_epoch {
            if epoch == 0 {
                0
            } else {
                // Consistent with get_epoch_and_slot_index warmup formula:
                // epoch N starts at (2^N - 1).  Guard shift for safety.
                1u64.checked_shl(epoch as u32)
                    .map(|v| v - 1)
                    .unwrap_or(u64::MAX)
            }
        } else {
            self.first_normal_slot + (epoch - self.first_normal_epoch) * self.slots_per_epoch
        }
    }

    pub fn get_last_slot_in_epoch(&self, epoch: u64) -> u64 {
        self.get_first_slot_in_epoch(epoch + 1) - 1
    }
}

impl Default for EpochSchedule {
    fn default() -> Self {
        Self::new(DEFAULT_SLOTS_PER_EPOCH)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_slots_per_epoch() {
        assert_eq!(DEFAULT_SLOTS_PER_EPOCH, 432_000);
    }

    #[test]
    fn default_slot_duration() {
        assert_eq!(DEFAULT_SLOT_DURATION_MS, 400);
    }

    #[test]
    fn epoch_schedule_basics() {
        let schedule = EpochSchedule::new(100);
        assert_eq!(schedule.get_epoch(0), 0);
        assert_eq!(schedule.get_epoch(99), 0);
        assert_eq!(schedule.get_epoch(100), 1);
        assert_eq!(schedule.get_epoch(199), 1);
    }

    #[test]
    fn epoch_and_slot_index() {
        let schedule = EpochSchedule::new(100);
        assert_eq!(schedule.get_epoch_and_slot_index(0), (0, 0));
        assert_eq!(schedule.get_epoch_and_slot_index(50), (0, 50));
        assert_eq!(schedule.get_epoch_and_slot_index(100), (1, 0));
        assert_eq!(schedule.get_epoch_and_slot_index(142), (1, 42));
    }

    #[test]
    fn first_and_last_slot() {
        let schedule = EpochSchedule::new(100);
        assert_eq!(schedule.get_first_slot_in_epoch(0), 0);
        assert_eq!(schedule.get_first_slot_in_epoch(1), 100);
        assert_eq!(schedule.get_last_slot_in_epoch(0), 99);
        assert_eq!(schedule.get_last_slot_in_epoch(1), 199);
    }

    #[test]
    fn borsh_roundtrip() {
        let schedule = EpochSchedule::default();
        let encoded = borsh::to_vec(&schedule).unwrap();
        let decoded: EpochSchedule = borsh::from_slice(&encoded).unwrap();
        assert_eq!(schedule, decoded);
    }
}
