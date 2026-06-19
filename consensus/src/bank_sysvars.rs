use std::sync::Arc;

use nusantara_crypto::Hash;
use nusantara_sysvar_program::{Clock, SlotHashes, StakeHistory, StakeHistoryEntry};
use tracing::instrument;

use crate::bank::ConsensusBank;

impl ConsensusBank {
    /// Maximum number of slot hash entries retained in the SlotHashes sysvar.
    const MAX_SLOT_HASHES: usize = 512;

    /// Advance to a new slot, updating the Clock sysvar.
    ///
    /// `Arc::make_mut` clones the inner `Clock` only when concurrent readers
    /// hold an outstanding `Arc<Clock>`. In the single-writer steady state the
    /// write is in-place, making this equivalent to a direct field assignment.
    #[instrument(skip(self), level = "debug")]
    pub fn advance_slot(&self, slot: u64, timestamp: i64) {
        *self.current_slot.write() = slot;

        let epoch = self.epoch_schedule.get_epoch(slot);
        let mut guard = self.clock.write();
        let clock = Arc::make_mut(&mut *guard);
        clock.slot = slot;
        clock.unix_timestamp = timestamp;
        clock.epoch = epoch;
        // B27: saturating_add prevents u64 overflow at far-future epochs.
        clock.leader_schedule_epoch = epoch.saturating_add(1);

        metrics::gauge!("nusantara_bank_current_slot").set(slot as f64);
    }

    /// Update slot hashes sysvar.
    /// Replaces any existing entry for the same slot (e.g. when an orphan block
    /// arrives and replaces a previously recorded skip Hash::zero()).
    ///
    /// Both `Vec::remove(pos)` and `Vec::insert(0, ...)` are O(N) element shifts.
    /// This is acceptable because SlotHashes is bounded at MAX_SLOT_HASHES=512
    /// entries, so the maximum shift is 512 elements. Changing the inner
    /// collection to `VecDeque` would break the Borsh wire format since
    /// `SlotHashes` derives its serialization from `Vec<(u64, Hash)>`.
    pub fn record_slot_hash(&self, slot: u64, hash: Hash) {
        let mut guard = self.slot_hashes.write();
        let slot_hashes = Arc::make_mut(&mut *guard);
        // Remove any existing entry for this slot (replace semantics).
        if let Some(pos) = slot_hashes.0.iter().position(|(s, _)| *s == slot) {
            slot_hashes.0.remove(pos);
        }
        slot_hashes.0.insert(0, (slot, hash));
        slot_hashes.0.truncate(Self::MAX_SLOT_HASHES);
    }

    /// Record a skipped slot in slot_hashes with Hash::zero().
    pub fn record_skipped_slot(&self, slot: u64) {
        self.record_slot_hash(slot, nusantara_crypto::Hash::zero());
    }

    /// Update stake history sysvar.
    pub fn update_stake_history(&self, epoch: u64, entry: StakeHistoryEntry) {
        let mut guard = self.stake_history.write();
        let history = Arc::make_mut(&mut *guard);
        history.0.insert(0, (epoch, entry));
        // Keep max 512 entries
        history.0.truncate(512);
    }

    /// Get the Clock sysvar.
    ///
    /// Returns an owned `Clock` by cloning out of the internal `Arc<Clock>`.
    /// In the common case where no writer holds the lock, this is a cheap
    /// Arc clone + deref-clone rather than a deep struct copy.
    pub fn clock(&self) -> Clock {
        (**self.clock.read()).clone()
    }

    /// Get the SlotHashes sysvar.
    pub fn slot_hashes(&self) -> SlotHashes {
        (**self.slot_hashes.read()).clone()
    }

    /// Replace slot_hashes entirely.
    ///
    /// Used during cross-fork block replay to match the block producer's
    /// `RecentBlockhashes` sysvar. Without this, the bank's slot_hashes
    /// reflects only this validator's own chain history, which diverges
    /// from the producer's chain when validators ran on separate forks.
    pub fn set_slot_hashes(&self, slot_hashes: SlotHashes) {
        *self.slot_hashes.write() = Arc::new(slot_hashes);
    }

    /// Get the StakeHistory sysvar.
    pub fn stake_history(&self) -> StakeHistory {
        (**self.stake_history.read()).clone()
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
