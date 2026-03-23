use nusantara_crypto::Hash;
use nusantara_sysvar_program::{Clock, SlotHashes, StakeHistory, StakeHistoryEntry};
use tracing::instrument;

use crate::bank::ConsensusBank;

impl ConsensusBank {
    /// Advance to a new slot, updating the Clock sysvar.
    #[instrument(skip(self), level = "debug")]
    pub fn advance_slot(&self, slot: u64, timestamp: i64) {
        *self.current_slot.write() = slot;

        let epoch = self.epoch_schedule.get_epoch(slot);
        let mut clock = self.clock.write();
        clock.slot = slot;
        clock.unix_timestamp = timestamp;
        clock.epoch = epoch;
        clock.leader_schedule_epoch = epoch + 1;

        metrics::gauge!("nusantara_bank_current_slot").set(slot as f64);
    }

    /// Update slot hashes sysvar.
    /// Replaces any existing entry for the same slot (e.g. when an orphan block
    /// arrives and replaces a previously recorded skip Hash::zero()).
    pub fn record_slot_hash(&self, slot: u64, hash: Hash) {
        let mut slot_hashes = self.slot_hashes.write();
        slot_hashes.0.retain(|(s, _)| *s != slot);
        slot_hashes.0.insert(0, (slot, hash));
        slot_hashes.0.truncate(512);
    }

    /// Record a skipped slot in slot_hashes with Hash::zero().
    pub fn record_skipped_slot(&self, slot: u64) {
        self.record_slot_hash(slot, nusantara_crypto::Hash::zero());
    }

    /// Update stake history sysvar.
    pub fn update_stake_history(&self, epoch: u64, entry: StakeHistoryEntry) {
        let mut history = self.stake_history.write();
        history.0.insert(0, (epoch, entry));
        // Keep max 512 entries
        history.0.truncate(512);
    }

    /// Get the Clock sysvar.
    pub fn clock(&self) -> Clock {
        self.clock.read().clone()
    }

    /// Get the SlotHashes sysvar.
    pub fn slot_hashes(&self) -> SlotHashes {
        self.slot_hashes.read().clone()
    }

    /// Replace slot_hashes entirely.
    ///
    /// Used during cross-fork block replay to match the block producer's
    /// `RecentBlockhashes` sysvar. Without this, the bank's slot_hashes
    /// reflects only this validator's own chain history, which diverges
    /// from the producer's chain when validators ran on separate forks.
    pub fn set_slot_hashes(&self, slot_hashes: SlotHashes) {
        *self.slot_hashes.write() = slot_hashes;
    }

    /// Get the StakeHistory sysvar.
    pub fn stake_history(&self) -> StakeHistory {
        self.stake_history.read().clone()
    }
}

#[cfg(test)]
mod tests {
    use crate::test_utils::test_helpers::temp_bank;

    #[test]
    fn advance_slot_updates_clock() {
        let (bank, _storage, _dir) = temp_bank();

        bank.advance_slot(42, 1234567890);
        let clock = bank.clock();
        assert_eq!(clock.slot, 42);
        assert_eq!(clock.unix_timestamp, 1234567890);
        assert_eq!(clock.epoch, 0); // 42 < 100 (slots_per_epoch)
    }

    #[test]
    fn record_slot_hash() {
        let (bank, _storage, _dir) = temp_bank();

        let h = nusantara_crypto::hash(b"block1");
        bank.record_slot_hash(1, h);
        let sh = bank.slot_hashes();
        assert_eq!(sh.get(1), Some(&h));
    }
}
