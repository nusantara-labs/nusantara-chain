use std::collections::BTreeMap;

use dashmap::DashMap;
use nusantara_core::block::Block;
use nusantara_core::native_token::const_parse_u64;
use nusantara_crypto::Hash;
use nusantara_storage::shred::DataShred;

use crate::deshredder::Deshredder;
use crate::merkle_shred::{MerkleDataShred, ShredBatchHeader};

pub const MAX_SHREDS_PER_SLOT: u64 =
    const_parse_u64(env!("NUSA_TURBINE_MAX_SHREDS_PER_SLOT"));

struct SlotShreds {
    /// Buffered shreds with their Merkle proofs, keyed by shred index.
    /// We store `MerkleDataShred` (not plain `DataShred`) so that proofs are
    /// available for retroactive verification when the header arrives later.
    data_shreds: BTreeMap<u32, MerkleDataShred>,
    last_index: Option<u32>,
    /// Cached batch header for this slot (contains Merkle root + signature).
    header: Option<ShredBatchHeader>,
}

impl SlotShreds {
    fn new() -> Self {
        Self {
            data_shreds: BTreeMap::new(),
            last_index: None,
            header: None,
        }
    }

    /// Insert a shred, enforcing the per-slot shred count limit.
    /// Returns `false` if the shred was rejected (duplicate, over limit, etc.).
    fn insert(&mut self, shred: &MerkleDataShred) -> bool {
        if shred.index() >= MAX_SHREDS_PER_SLOT as u32 {
            metrics::counter!("nusantara_turbine_shreds_rejected_max_index").increment(1);
            return false;
        }
        if self.data_shreds.len() >= MAX_SHREDS_PER_SLOT as usize {
            metrics::counter!("nusantara_turbine_shreds_rejected_max_index").increment(1);
            return false;
        }
        // Skip duplicate shreds — avoids redundant clones and overwrites
        if self.data_shreds.contains_key(&shred.index()) {
            metrics::counter!("nusantara_turbine_shreds_duplicate_skipped").increment(1);
            return false;
        }
        if shred.is_last() {
            self.last_index = Some(shred.index());
        }
        self.data_shreds.insert(shred.index(), shred.clone());
        true
    }

    /// A slot is complete only when:
    /// 1. We have received the batch header (with merkle root for verification)
    /// 2. We have received all data shreds up to and including the last one
    fn is_complete(&self) -> bool {
        if self.header.is_none() {
            return false;
        }
        let last = match self.last_index {
            Some(l) => l,
            None => return false,
        };
        self.data_shreds.len() == (last + 1) as usize
    }

    /// Retroactively verify all buffered shreds against the merkle root.
    /// Evicts any shred whose Merkle proof does not verify.
    /// Returns the number of shreds evicted.
    fn verify_buffered_shreds(&mut self, merkle_root: &Hash) -> usize {
        let invalid_indices: Vec<u32> = self
            .data_shreds
            .iter()
            .filter(|(_, shred)| !shred.verify(merkle_root))
            .map(|(&idx, _)| idx)
            .collect();

        let evicted = invalid_indices.len();
        for idx in &invalid_indices {
            self.data_shreds.remove(idx);
        }

        // If the last-flagged shred was evicted, clear last_index so
        // is_complete() cannot return true with a stale value.
        if let Some(last) = self.last_index
            && invalid_indices.contains(&last)
        {
            self.last_index = None;
        }

        evicted
    }

    fn to_sorted_shreds(&self) -> Vec<DataShred> {
        self.data_shreds.values().map(|m| m.shred.clone()).collect()
    }
}

pub struct ShredCollector {
    slots: DashMap<u64, SlotShreds>,
    stored_slots: DashMap<u64, ()>,
    /// Slots known to be empty/skipped — blocks `request_slot_repair()` but
    /// NOT `insert_data_shred()` or `insert_header()`, so turbine can still
    /// deliver shreds if the slot turns out to have a block.
    skip_repair_slots: DashMap<u64, ()>,
}

impl ShredCollector {
    pub fn new() -> Self {
        Self {
            slots: DashMap::new(),
            stored_slots: DashMap::new(),
            skip_repair_slots: DashMap::new(),
        }
    }

    pub fn mark_slot_stored(&self, slot: u64) {
        self.stored_slots.insert(slot, ());
        self.slots.remove(&slot);
    }

    /// Mark a slot as "known empty" — repair won't re-request it, but
    /// turbine shreds are still accepted if the slot has a block.
    pub fn mark_slot_empty(&self, slot: u64) {
        self.skip_repair_slots.insert(slot, ());
    }

    pub fn is_slot_stored(&self, slot: u64) -> bool {
        self.stored_slots.contains_key(&slot)
    }

    /// Insert a `ShredBatchHeader`.
    ///
    /// When shreds have arrived before the header, this retroactively verifies
    /// all buffered shreds against the header's merkle root and evicts any that
    /// fail verification. If all shreds are present and valid after verification,
    /// the block is assembled and returned.
    pub fn insert_header(&self, header: ShredBatchHeader) -> Option<Block> {
        let slot = header.slot;
        if self.stored_slots.contains_key(&slot) {
            return None;
        }

        let merkle_root = header.merkle_root;
        let mut entry = self.slots.entry(slot).or_insert_with(SlotShreds::new);
        entry.header = Some(header);

        // Retroactively verify all buffered shreds against the merkle root.
        let evicted = entry.verify_buffered_shreds(&merkle_root);
        if evicted > 0 {
            metrics::counter!("nusantara_turbine_invalid_shred_signatures").increment(evicted as u64);
            tracing::warn!(
                slot,
                evicted,
                "evicted buffered shreds that failed retroactive merkle verification"
            );
        }

        // Check if the slot is now complete after verification
        if entry.is_complete() {
            let sorted = entry.to_sorted_shreds();
            drop(entry);
            match Deshredder::deshred(&sorted) {
                Ok(block) => {
                    self.stored_slots.insert(slot, ());
                    self.slots.remove(&slot);
                    metrics::counter!("nusantara_turbine_blocks_assembled_total").increment(1);
                    return Some(block);
                }
                Err(e) => {
                    tracing::warn!(slot, error = %e, "deshredding failed after header insertion");
                }
            }
        }

        None
    }

    /// Get the cached Merkle root for a slot (if header has been received).
    pub fn get_merkle_root(&self, slot: u64) -> Option<Hash> {
        self.slots
            .get(&slot)
            .and_then(|e| e.header.as_ref().map(|h| h.merkle_root))
    }

    /// Check if we have the batch header for a slot.
    pub fn has_header(&self, slot: u64) -> bool {
        self.slots
            .get(&slot)
            .is_some_and(|e| e.header.is_some())
    }

    /// Insert a Merkle data shred. Returns `Some(Block)` if the slot is now complete.
    ///
    /// If the batch header has not yet arrived, the shred is buffered without
    /// Merkle verification and block assembly is deferred until `insert_header`
    /// provides the merkle root. This prevents an attacker from injecting
    /// forged shreds that bypass proof verification.
    pub fn insert_data_shred(&self, shred: &MerkleDataShred) -> Option<Block> {
        let slot = shred.slot();

        if self.stored_slots.contains_key(&slot) {
            metrics::counter!("nusantara_turbine_shreds_skipped_already_stored").increment(1);
            return None;
        }

        let mut entry = self.slots.entry(slot).or_insert_with(SlotShreds::new);

        // If header is present, verify Merkle proof before accepting
        if let Some(ref header) = entry.header
            && !shred.verify(&header.merkle_root)
        {
            metrics::counter!("nusantara_turbine_invalid_shred_signatures").increment(1);
            return None;
        }

        if !entry.insert(shred) {
            return None;
        }

        // is_complete() returns false when header is None, so block assembly
        // only happens after the header (and its merkle root) are available.
        if entry.is_complete() {
            let sorted = entry.to_sorted_shreds();
            drop(entry);
            match Deshredder::deshred(&sorted) {
                Ok(block) => {
                    // Mark as stored BEFORE removing shred data so duplicate
                    // shreds arriving concurrently are rejected immediately.
                    self.stored_slots.insert(slot, ());
                    self.slots.remove(&slot);
                    metrics::counter!("nusantara_turbine_blocks_assembled_total").increment(1);
                    Some(block)
                }
                Err(e) => {
                    tracing::warn!(slot, error = %e, "deshredding failed");
                    None
                }
            }
        } else {
            None
        }
    }

    pub fn missing_shreds(&self, slot: u64) -> Vec<u32> {
        let entry = match self.slots.get(&slot) {
            Some(e) => e,
            None => return Vec::new(),
        };

        let last = match entry.last_index {
            Some(l) => l,
            None => return Vec::new(),
        };

        (0..=last)
            .filter(|i| !entry.data_shreds.contains_key(i))
            .collect()
    }

    pub fn has_slot(&self, slot: u64) -> bool {
        self.slots.contains_key(&slot)
    }

    pub fn shred_count(&self, slot: u64) -> usize {
        self.slots
            .get(&slot)
            .map(|e| e.data_shreds.len())
            .unwrap_or(0)
    }

    pub fn is_slot_complete(&self, slot: u64) -> bool {
        self.slots
            .get(&slot)
            .is_some_and(|e| e.is_complete())
    }

    pub fn remove_slot(&self, slot: u64) {
        self.slots.remove(&slot);
    }

    pub fn request_slot_repair(&self, slot: u64) {
        if self.stored_slots.contains_key(&slot) {
            return;
        }
        if self.skip_repair_slots.contains_key(&slot) {
            return;
        }
        self.slots.entry(slot).or_insert_with(SlotShreds::new);
    }

    pub fn cleanup_old_slots(&self, current_slot: u64, max_age: u64) -> usize {
        let cutoff = current_slot.saturating_sub(max_age);
        let old_slots: Vec<u64> = self
            .slots
            .iter()
            .filter(|e| *e.key() < cutoff)
            .map(|e| *e.key())
            .collect();
        let count = old_slots.len();
        for slot in old_slots {
            self.slots.remove(&slot);
        }

        let old_stored: Vec<u64> = self
            .stored_slots
            .iter()
            .filter(|e| *e.key() < cutoff)
            .map(|e| *e.key())
            .collect();
        for slot in old_stored {
            self.stored_slots.remove(&slot);
        }

        let old_skip: Vec<u64> = self
            .skip_repair_slots
            .iter()
            .filter(|e| *e.key() < cutoff)
            .map(|e| *e.key())
            .collect();
        for slot in old_skip {
            self.skip_repair_slots.remove(&slot);
        }

        if count > 0 {
            metrics::counter!("nusantara_turbine_shred_collector_slots_evicted").increment(count as u64);
        }
        count
    }

    pub fn tracked_slots(&self) -> Vec<u64> {
        self.slots.iter().map(|e| *e.key()).collect()
    }
}

impl Default for ShredCollector {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shredder::Shredder;
    use nusantara_core::block::{Block, BlockHeader};
    use nusantara_crypto::{Hash, Keypair, hash};

    fn test_block(slot: u64) -> Block {
        Block {
            header: BlockHeader {
                slot,
                parent_slot: slot.saturating_sub(1),
                parent_hash: hash(b"parent"),
                block_hash: hash(b"block"),
                timestamp: 1000,
                validator: hash(b"validator"),
                transaction_count: 0,
                merkle_root: Hash::zero(),
                poh_hash: Hash::zero(),
                bank_hash: Hash::zero(),
                state_root: Hash::zero(),
            },
            transactions: Vec::new(),
            batches: Vec::new(),
        }
    }

    #[test]
    fn collect_all_shreds_assembles_block() {
        let kp = Keypair::generate();
        let block = test_block(1);
        let batch = Shredder::shred_block(&block, 0, &kp).unwrap();
        let collector = ShredCollector::new();

        // Insert header first
        collector.insert_header(batch.header.clone());

        let mut result = None;
        for shred in &batch.data_shreds {
            if let Some(assembled) = collector.insert_data_shred(shred) {
                result = Some(assembled);
            }
        }

        assert!(result.is_some());
        assert_eq!(result.unwrap(), block);
    }

    #[test]
    fn incomplete_slot_returns_none() {
        let kp = Keypair::generate();
        let block = test_block(1);
        let batch = Shredder::shred_block(&block, 0, &kp).unwrap();

        if batch.data_shreds.len() > 1 {
            let collector = ShredCollector::new();
            // Insert header so is_complete() can potentially return true
            collector.insert_header(batch.header.clone());
            for shred in &batch.data_shreds[..batch.data_shreds.len() - 1] {
                assert!(collector.insert_data_shred(shred).is_none());
            }
        }
    }

    #[test]
    fn missing_shreds_detection() {
        let kp = Keypair::generate();
        let block = test_block(1);
        let batch = Shredder::shred_block(&block, 0, &kp).unwrap();
        let collector = ShredCollector::new();

        if batch.data_shreds.len() > 2 {
            collector.insert_data_shred(&batch.data_shreds[0]);
            collector.insert_data_shred(batch.data_shreds.last().unwrap());

            let missing = collector.missing_shreds(1);
            assert!(!missing.is_empty());
        }
    }

    #[test]
    fn stored_slot_skips_insertion() {
        let kp = Keypair::generate();
        let block = test_block(1);
        let batch = Shredder::shred_block(&block, 0, &kp).unwrap();
        let collector = ShredCollector::new();

        collector.mark_slot_stored(1);
        assert!(collector.is_slot_stored(1));

        for shred in &batch.data_shreds {
            assert!(collector.insert_data_shred(shred).is_none());
        }

        assert!(!collector.has_slot(1));
    }

    #[test]
    fn stored_slot_skips_repair_request() {
        let collector = ShredCollector::new();
        collector.mark_slot_stored(5);

        collector.request_slot_repair(5);
        assert!(!collector.has_slot(5));
    }

    #[test]
    fn rejects_shred_above_max_index() {
        let kp = Keypair::generate();
        let collector = ShredCollector::new();

        let shred = nusantara_storage::shred::DataShred {
            slot: 1,
            index: MAX_SHREDS_PER_SLOT as u32,
            parent_offset: 1,
            data: vec![0u8; 10],
            flags: 0,
        };
        let merkle = MerkleDataShred::new(shred, kp.address());
        assert!(collector.insert_data_shred(&merkle).is_none());
    }

    #[test]
    fn accepts_shred_within_limit() {
        let kp = Keypair::generate();
        let collector = ShredCollector::new();

        let shred = nusantara_storage::shred::DataShred {
            slot: 1,
            index: 0,
            parent_offset: 1,
            data: vec![0u8; 10],
            flags: 0x01,
        };
        let merkle = MerkleDataShred::new(shred, kp.address());
        let _ = collector.insert_data_shred(&merkle);
        assert!(collector.has_slot(1) || collector.is_slot_stored(1));
    }

    #[test]
    fn cleanup_evicts_old_stored_slots() {
        let collector = ShredCollector::new();
        collector.mark_slot_stored(10);
        collector.mark_slot_stored(50);
        collector.mark_slot_stored(90);

        collector.cleanup_old_slots(100, 50);

        assert!(!collector.is_slot_stored(10));
        assert!(collector.is_slot_stored(50));
        assert!(collector.is_slot_stored(90));
    }

    #[test]
    fn header_insert_and_lookup() {
        let kp = Keypair::generate();
        let root = hash(b"root");
        let header = ShredBatchHeader {
            slot: 5,
            leader: kp.address(),
            merkle_root: root,
            signature: kp.sign(root.as_bytes()),
            num_data_shreds: 10,
            num_code_shreds: 3,
        };
        let collector = ShredCollector::new();
        collector.insert_header(header);

        assert!(collector.has_header(5));
        assert_eq!(collector.get_merkle_root(5), Some(root));
        assert!(!collector.has_header(6));
    }

    /// Shreds arriving before the header must be buffered but must NOT trigger
    /// block assembly. Assembly should only happen after the header arrives
    /// and all buffered shreds pass retroactive Merkle verification.
    #[test]
    fn shreds_before_header_waits_for_header() {
        let kp = Keypair::generate();
        let block = test_block(1);
        let batch = Shredder::shred_block(&block, 0, &kp).unwrap();
        let collector = ShredCollector::new();

        // Insert ALL shreds without header first — no block should be assembled
        for shred in &batch.data_shreds {
            let result = collector.insert_data_shred(shred);
            assert!(
                result.is_none(),
                "block assembly must not happen without header"
            );
        }

        // Shreds are buffered
        assert!(collector.has_slot(1));
        assert_eq!(collector.shred_count(1), batch.data_shreds.len());
        // Slot should NOT be marked complete (header missing)
        assert!(!collector.is_slot_complete(1));

        // Now insert header — retroactive verification + assembly
        let assembled = collector.insert_header(batch.header.clone());
        assert!(assembled.is_some(), "block should assemble after header");
        assert_eq!(assembled.unwrap(), block);
    }

    /// Forged shreds (with invalid/empty Merkle proofs) that arrived before
    /// the header must be evicted during retroactive verification when the
    /// header arrives.
    #[test]
    fn forged_shreds_rejected_after_header_arrival() {
        let kp = Keypair::generate();
        let block = test_block(1);
        let batch = Shredder::shred_block(&block, 0, &kp).unwrap();
        let collector = ShredCollector::new();

        // Create a forged shred with a fake proof
        let forged_data = nusantara_storage::shred::DataShred {
            slot: 1,
            index: 0,
            parent_offset: 1,
            data: vec![0xDE, 0xAD, 0xBE, 0xEF],
            flags: 0,
        };
        let forged_shred = MerkleDataShred::new(forged_data, kp.address());

        // Insert the forged shred before header — it gets buffered
        collector.insert_data_shred(&forged_shred);
        assert_eq!(collector.shred_count(1), 1);

        // Insert the legitimate shreds too (except index 0 which is already forged)
        for shred in &batch.data_shreds {
            if shred.index() != 0 {
                collector.insert_data_shred(shred);
            }
        }

        // Insert the real header — forged shred at index 0 should be evicted
        let result = collector.insert_header(batch.header.clone());

        // Block should NOT assemble because the forged shred at index 0 was
        // evicted, leaving a gap.
        assert!(
            result.is_none(),
            "block must not assemble when forged shreds are present"
        );

        // Verify forged shred was evicted
        if let Some(entry) = collector.slots.get(&1) {
            assert!(
                !entry.data_shreds.contains_key(&0),
                "forged shred at index 0 should have been evicted"
            );
        }
    }

    /// When header is present, a shred with an invalid Merkle proof is
    /// rejected immediately (not buffered).
    #[test]
    fn invalid_proof_rejected_with_header_present() {
        let kp = Keypair::generate();
        let block = test_block(1);
        let batch = Shredder::shred_block(&block, 0, &kp).unwrap();
        let collector = ShredCollector::new();

        // Insert header first
        collector.insert_header(batch.header.clone());

        // Create a shred with invalid proof
        let bad_data = nusantara_storage::shred::DataShred {
            slot: 1,
            index: 99,
            parent_offset: 1,
            data: vec![0xFF; 10],
            flags: 0,
        };
        let bad_shred = MerkleDataShred::new(bad_data, kp.address());

        // Should be rejected because proof (empty) does not verify
        let result = collector.insert_data_shred(&bad_shred);
        assert!(result.is_none());

        // The bad shred should not be in the collector
        assert!(collector.shred_count(1) == 0 || {
            let entry = collector.slots.get(&1).unwrap();
            !entry.data_shreds.contains_key(&99)
        });
    }

    /// Verify that inserting header for an already-stored slot is a no-op.
    #[test]
    fn header_insert_for_stored_slot_is_noop() {
        let kp = Keypair::generate();
        let root = hash(b"root");
        let header = ShredBatchHeader {
            slot: 5,
            leader: kp.address(),
            merkle_root: root,
            signature: kp.sign(root.as_bytes()),
            num_data_shreds: 10,
            num_code_shreds: 3,
        };
        let collector = ShredCollector::new();
        collector.mark_slot_stored(5);

        let result = collector.insert_header(header);
        assert!(result.is_none());
        assert!(!collector.has_header(5));
    }

    /// Full flow: header first, then shreds — the standard path.
    #[test]
    fn header_first_then_shreds_assembles() {
        let kp = Keypair::generate();
        let block = test_block(2);
        let batch = Shredder::shred_block(&block, 1, &kp).unwrap();
        let collector = ShredCollector::new();

        // Header first
        let result = collector.insert_header(batch.header.clone());
        assert!(result.is_none(), "no shreds yet, no assembly");

        // Then insert shreds
        let mut assembled = None;
        for shred in &batch.data_shreds {
            if let Some(b) = collector.insert_data_shred(shred) {
                assembled = Some(b);
            }
        }

        assert!(assembled.is_some());
        assert_eq!(assembled.unwrap(), block);
    }

    /// Inserting the same shred twice should return false on the second insert
    /// (duplicate skipped).
    #[test]
    fn duplicate_shred_rejected() {
        let kp = Keypair::generate();
        let collector = ShredCollector::new();

        let shred = nusantara_storage::shred::DataShred {
            slot: 1,
            index: 0,
            parent_offset: 1,
            data: vec![0u8; 10],
            flags: 0,
        };
        let merkle = MerkleDataShred::new(shred, kp.address());

        // First insert succeeds (no header, so it gets buffered)
        let r1 = collector.insert_data_shred(&merkle);
        assert!(r1.is_none()); // buffered, not assembled
        assert_eq!(collector.shred_count(1), 1);

        // Second insert of same shred should be skipped (duplicate)
        let r2 = collector.insert_data_shred(&merkle);
        assert!(r2.is_none());
        assert_eq!(collector.shred_count(1), 1); // still 1, not 2
    }
}
