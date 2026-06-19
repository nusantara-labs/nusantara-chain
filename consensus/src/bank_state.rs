use std::time::Instant;

use nusantara_core::Account;
use nusantara_crypto::{Hash, hashv};
use nusantara_storage::Storage;
use tracing::instrument;

use crate::bank::{ConsensusBank, FrozenBankState};
use crate::error::ConsensusError;
use crate::state_tree::{StateMerkleProof, StateTree};

impl ConsensusBank {
    /// Compute the bank hash from parent hash and account delta hash.
    pub fn compute_bank_hash(parent_bank_hash: &Hash, account_delta_hash: &Hash) -> Hash {
        hashv(&[parent_bank_hash.as_bytes(), account_delta_hash.as_bytes()])
    }

    /// Freeze the bank state for the current slot.
    #[instrument(skip(self), level = "info")]
    pub fn freeze(
        &self,
        slot: u64,
        parent_slot: u64,
        block_hash: Hash,
        parent_bank_hash: &Hash,
        account_delta_hash: &Hash,
        transaction_count: u64,
    ) -> FrozenBankState {
        let bank_hash = Self::compute_bank_hash(parent_bank_hash, account_delta_hash);
        let epoch = self.epoch_schedule.get_epoch(slot);

        metrics::counter!("nusantara_bank_slots_frozen_total").increment(1);

        FrozenBankState {
            slot,
            parent_slot,
            block_hash,
            bank_hash,
            epoch,
            transaction_count,
        }
    }

    /// Persist critical state to storage.
    #[instrument(skip(self), level = "info")]
    pub fn flush_to_storage(&self, frozen: &FrozenBankState) -> Result<(), ConsensusError> {
        self.storage.put_bank_hash(frozen.slot, &frozen.bank_hash)?;
        self.storage
            .put_slot_hash(frozen.slot, &frozen.block_hash)?;
        Ok(())
    }

    /// Mark a slot as a finalized root in storage.
    pub fn set_root(&self, slot: u64) -> Result<(), ConsensusError> {
        self.storage.set_root(slot)?;
        Ok(())
    }

    /// Rollback bank state to a given ancestor slot.
    /// Resets the clock and current_slot to the ancestor.
    pub fn rollback_to_slot(&self, slot: u64, storage: &Storage) -> Result<(), ConsensusError> {
        // Reset current_slot
        *self.current_slot.write() = slot;

        // Try to get block header to restore timestamp
        if let Some(header) = storage.get_block_header(slot)? {
            let epoch = self.epoch_schedule.get_epoch(slot);
            let mut guard = self.clock.write();
            let clock = std::sync::Arc::make_mut(&mut *guard);
            clock.slot = slot;
            clock.unix_timestamp = header.timestamp;
            clock.epoch = epoch;
            clock.leader_schedule_epoch = epoch.saturating_add(1);
        }

        // Rebuild slot_hashes: keep only entries at or before the target slot
        let mut guard = self.slot_hashes.write();
        let slot_hashes = std::sync::Arc::make_mut(&mut *guard);
        slot_hashes.0.retain(|(s, _)| *s <= slot);

        Ok(())
    }

    /// Update the state Merkle tree with account deltas from a slot execution.
    ///
    /// This should be called after committing account deltas to storage
    /// so the state root reflects the latest on-chain state.
    #[instrument(skip_all, fields(delta_count = deltas.len()), level = "debug")]
    pub fn update_state_tree(&self, deltas: &[(Hash, Account)]) {
        let start = Instant::now();
        let mut tree = self.state_tree.lock();
        tree.update(deltas);
        let elapsed = start.elapsed();
        metrics::gauge!("nusantara_state_tree_leaf_count").set(tree.len() as f64);
        metrics::histogram!("nusantara_state_tree_update_duration_ms")
            .record(elapsed.as_secs_f64() * 1000.0);
        tracing::debug!(
            elapsed_us = elapsed.as_micros(),
            delta_count = deltas.len(),
            leaf_count = tree.len(),
            "state tree update complete"
        );
    }

    /// Compute the current state Merkle root.
    ///
    /// If only existing accounts were modified since the last root computation,
    /// this uses incremental dirty-path recomputation (O(D * log N)).
    /// Structural changes trigger a full O(N log N) rebuild.
    pub fn state_root(&self) -> Hash {
        let start = Instant::now();
        let root = self.state_tree.lock().root();
        let elapsed = start.elapsed();
        metrics::histogram!("nusantara_state_tree_root_duration_ms")
            .record(elapsed.as_secs_f64() * 1000.0);
        tracing::debug!(
            elapsed_us = elapsed.as_micros(),
            "state root computation complete"
        );
        root
    }

    /// Number of accounts tracked in the state tree.
    pub fn state_tree_len(&self) -> usize {
        self.state_tree.lock().len()
    }

    /// Generate a state Merkle proof for a specific account.
    pub fn state_proof(&self, address: &Hash) -> Option<StateMerkleProof> {
        self.state_tree.lock().proof(address)
    }

    /// Replace the state tree (e.g., after loading from storage at boot).
    pub fn set_state_tree(&self, tree: StateTree) {
        *self.state_tree.lock() = tree;
    }
}

#[cfg(test)]
mod tests {
    use crate::bank::ConsensusBank;
    use crate::test_utils::test_helpers::temp_bank;

    #[test]
    fn freeze_and_flush() {
        let (bank, _storage, _dir) = temp_bank();

        let block_hash = nusantara_crypto::hash(b"block");
        let parent_bank = nusantara_crypto::hash(b"parent_bank");
        let delta = nusantara_crypto::hash(b"delta");

        let frozen = bank.freeze(1, 0, block_hash, &parent_bank, &delta, 10);
        assert_eq!(frozen.slot, 1);
        assert_eq!(frozen.transaction_count, 10);

        bank.flush_to_storage(&frozen).unwrap();
        let stored_bank_hash = bank.storage().get_bank_hash(1).unwrap();
        assert_eq!(stored_bank_hash, Some(frozen.bank_hash));
    }

    #[test]
    fn compute_bank_hash_deterministic() {
        let p = nusantara_crypto::hash(b"parent");
        let d = nusantara_crypto::hash(b"delta");
        let h1 = ConsensusBank::compute_bank_hash(&p, &d);
        let h2 = ConsensusBank::compute_bank_hash(&p, &d);
        assert_eq!(h1, h2);
    }
}
