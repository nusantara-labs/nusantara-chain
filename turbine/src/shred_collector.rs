use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};

use dashmap::DashMap;
use dashmap::mapref::entry::Entry;
use nusantara_core::block::Block;
use nusantara_core::native_token::const_parse_u64;
use nusantara_crypto::Hash;
use nusantara_storage::shred::DataShred;

use crate::deshredder::Deshredder;
use crate::merkle_shred::{MerkleCodeShred, MerkleDataShred, ShredBatchHeader};

pub const MAX_SHREDS_PER_SLOT: u64 =
    const_parse_u64(env!("NUSA_TURBINE_MAX_SHREDS_PER_SLOT"));

/// Maximum number of in-flight slots the collector will track simultaneously.
/// Slots beyond this limit are dropped (oldest-first) to prevent unbounded memory growth.
pub const MAX_TRACKED_SLOTS: usize =
    const_parse_u64(env!("NUSA_TURBINE_MAX_TRACKED_SLOTS")) as usize;

struct SlotShreds {
    /// Buffered data shreds with their Merkle proofs, keyed by shred index.
    /// We store `MerkleDataShred` (not plain `DataShred`) so that proofs are
    /// available for retroactive verification when the header arrives later.
    data_shreds: BTreeMap<u32, MerkleDataShred>,
    /// Buffered code (FEC) shreds, keyed by shred index.
    ///
    /// Code shreds that arrive before the batch header cannot be Merkle-verified
    /// yet — they are stored here for:
    ///   1. Retroactive Merkle verification when the header arrives.
    ///   2. Future FEC recovery work (not implemented in this pass — see NOTE).
    ///
    /// NOTE: FEC recovery from buffered code shreds is out of scope for this fix.
    /// The sole goal here is to stop silently dropping code shreds on the
    /// pre-header path. FEC recovery integration is a separate future improvement.
    code_shreds: BTreeMap<u32, MerkleCodeShred>,
    last_index: Option<u32>,
    /// Cached batch header for this slot (contains Merkle root + signature).
    header: Option<ShredBatchHeader>,
}

impl SlotShreds {
    fn new() -> Self {
        Self {
            data_shreds: BTreeMap::new(),
            code_shreds: BTreeMap::new(),
            last_index: None,
            header: None,
        }
    }

    /// Insert a data shred, enforcing the per-slot **combined** shred budget.
    ///
    /// # Memory budget
    /// `data_shreds` and `code_shreds` share a single `MAX_SHREDS_PER_SLOT`
    /// budget. Enforcing separate per-map caps would allow a slot to hold
    /// `2 * MAX_SHREDS_PER_SLOT` objects — doubling the per-slot memory bound
    /// without any corresponding increase in `MAX_TRACKED_SLOTS` to compensate.
    ///
    /// Returns `false` if the shred was rejected (duplicate, over limit, etc.).
    fn insert(&mut self, shred: &MerkleDataShred) -> bool {
        if shred.index() >= MAX_SHREDS_PER_SLOT as u32 {
            metrics::counter!("nusantara_turbine_shreds_rejected_max_index").increment(1);
            return false;
        }
        // Combined budget: data + code shreds share the per-slot cap.
        if self.data_shreds.len() + self.code_shreds.len() >= MAX_SHREDS_PER_SLOT as usize {
            metrics::counter!("nusantara_turbine_shreds_rejected_map_full").increment(1);
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

    /// Insert a code (FEC) shred, enforcing the per-slot **combined** shred budget.
    ///
    /// # Memory budget
    /// Shares `MAX_SHREDS_PER_SLOT` with `data_shreds` — see `insert` doc.
    /// The index cap (`shred.index() < MAX_SHREDS_PER_SLOT`) is still per-map
    /// (data indices are independent of code indices in the shred layout).
    ///
    /// Code shreds arriving pre-header are buffered for retroactive Merkle
    /// verification (which runs in `verify_buffered_code_shreds` when the header
    /// arrives). FEC recovery from these buffered shreds is a future improvement.
    ///
    /// Returns `false` if the shred was rejected (duplicate or over combined limit).
    fn insert_code(&mut self, shred: &MerkleCodeShred) -> bool {
        if shred.index() >= MAX_SHREDS_PER_SLOT as u32 {
            metrics::counter!("nusantara_turbine_shreds_rejected_max_index").increment(1);
            return false;
        }
        // Combined budget: data + code shreds share the per-slot cap.
        if self.data_shreds.len() + self.code_shreds.len() >= MAX_SHREDS_PER_SLOT as usize {
            metrics::counter!("nusantara_turbine_shreds_rejected_map_full").increment(1);
            return false;
        }
        if self.code_shreds.contains_key(&shred.index()) {
            metrics::counter!("nusantara_turbine_shreds_duplicate_skipped").increment(1);
            return false;
        }
        self.code_shreds.insert(shred.index(), shred.clone());
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

    /// Retroactively verify all buffered data shreds against the Merkle root.
    /// Evicts any shred whose proof does not verify.
    /// Returns the number of data shreds evicted.
    fn verify_buffered_data_shreds(&mut self, merkle_root: &Hash) -> usize {
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

    /// Retroactively verify all buffered code (FEC) shreds against the Merkle root.
    /// Evicts any shred whose proof does not verify.
    /// Returns the number of code shreds evicted.
    fn verify_buffered_code_shreds(&mut self, merkle_root: &Hash) -> usize {
        let invalid_indices: Vec<u32> = self
            .code_shreds
            .iter()
            .filter(|(_, shred)| !shred.verify(merkle_root))
            .map(|(&idx, _)| idx)
            .collect();

        let evicted = invalid_indices.len();
        for idx in &invalid_indices {
            self.code_shreds.remove(idx);
        }

        evicted
    }

    /// Consume and return sorted data shreds, leaving the map empty.
    /// Moves `shred` out of the owned `MerkleDataShred` — no clone needed.
    fn take_sorted_shreds(&mut self) -> Vec<DataShred> {
        std::mem::take(&mut self.data_shreds)
            .into_values()
            .map(|m| m.shred)
            .collect()
    }
}

pub struct ShredCollector {
    slots: DashMap<u64, SlotShreds>,
    stored_slots: DashMap<u64, ()>,
    /// Slots known to be empty/skipped — blocks `request_slot_repair()` but
    /// NOT `insert_data_shred()` or `insert_header()`, so turbine can still
    /// deliver shreds if the slot turns out to have a block.
    skip_repair_slots: DashMap<u64, ()>,
    /// Atomic slot count — bounds the cap check without TOCTOU on `slots.len()`.
    ///
    /// Uses the same pattern as `tpu-forward::connection_cache::ConnectionCache`:
    /// the `Entry`-API insert in `try_insert_slot` holds the DashMap shard lock
    /// across the count increment so two concurrent first-inserts of the same
    /// slot cannot both increment the counter. May transiently exceed
    /// `MAX_TRACKED_SLOTS` by at most the concurrency of distinct-slot inserts;
    /// `cleanup_old_slots` resyncs it to the true map size.
    slot_count: AtomicUsize,
}

impl ShredCollector {
    pub fn new() -> Self {
        Self {
            slots: DashMap::new(),
            stored_slots: DashMap::new(),
            skip_repair_slots: DashMap::new(),
            slot_count: AtomicUsize::new(0),
        }
    }

    pub fn mark_slot_stored(&self, slot: u64) {
        self.stored_slots.insert(slot, ());
        if self.slots.remove(&slot).is_some() {
            self.account_removed_slots(1);
        }
    }

    /// Mark a slot as "known empty" — repair won't re-request it, but
    /// turbine shreds are still accepted if the slot has a block.
    pub fn mark_slot_empty(&self, slot: u64) {
        self.skip_repair_slots.insert(slot, ());
    }

    pub fn is_slot_stored(&self, slot: u64) -> bool {
        self.stored_slots.contains_key(&slot)
    }

    /// Evict the oldest slot to make room. Returns `true` if a slot was evicted.
    fn evict_oldest_slot(&self) -> bool {
        // O(N) scan but only runs when the cap is hit — rare in normal operation.
        if let Some(oldest) = self.slots.iter().map(|e| *e.key()).min()
            && self.slots.remove(&oldest).is_some()
        {
            // Decrement to stay consistent with the true map size.
            self.slot_count.fetch_sub(1, Ordering::Relaxed);
            metrics::counter!("nusantara_turbine_slots_evicted_cap").increment(1);
            tracing::warn!(
                evicted_slot = oldest,
                cap = MAX_TRACKED_SLOTS,
                "slot tracking cap reached, evicted oldest slot"
            );
            return true;
        }
        false
    }

    /// Insert a new slot entry, enforcing the cap without holding any DashMap
    /// shard lock during eviction.
    ///
    /// # Deadlock prevention
    /// DashMap `Entry` holds the shard lock for `slot` until the `Entry` is
    /// dropped. `evict_oldest_slot` calls `self.slots.iter()` which traverses
    /// ALL shards — if it reaches the shard already locked by the `Entry`, it
    /// will deadlock. To prevent this:
    ///
    /// 1. Check capacity and evict BEFORE acquiring the `Entry` (no shard lock held).
    /// 2. Only then call `self.slots.entry(slot)` to get the `Entry`.
    ///
    /// # Cap enforcement guarantee
    /// The eviction loop runs until either the slot already exists in the map
    /// (Occupied path — no new entry needed) OR `slot_count` is below the cap.
    /// Two concurrent inserts of *different* new slots can transiently push
    /// `slot_count` above `MAX_TRACKED_SLOTS` by at most the degree of concurrent
    /// distinct-slot insertions — the same best-effort guarantee as
    /// `tpu-forward::connection_cache::ConnectionCache`. The atomic counter is
    /// resynced to the true map size on every `cleanup_old_slots` tick.
    ///
    /// # cleanup_old_slots contract
    /// Callers MUST schedule `cleanup_old_slots` on a regular tick (e.g. via
    /// `repair_service::RepairService::run`) so that the atomic counter is
    /// periodically resynced and stale slots are evicted. Without this the cap
    /// can drift permanently above `MAX_TRACKED_SLOTS`.
    fn get_or_insert_slot(&self, slot: u64) -> dashmap::mapref::one::RefMut<'_, u64, SlotShreds> {
        // Phase 1: evict while holding NO shard locks.
        // Loop until the slot is already present (no new entry needed) OR the
        // counter is below cap. This closes the window where two threads inserting
        // *different* new slots both pass a single cap check simultaneously.
        const MAX_EVICT_ATTEMPTS: usize = 8;
        if !self.slots.contains_key(&slot) {
            let mut attempts = 0;
            while self.slot_count.load(Ordering::Acquire) >= MAX_TRACKED_SLOTS
                && attempts < MAX_EVICT_ATTEMPTS
            {
                if !self.evict_oldest_slot() {
                    // Nothing left to evict (map may be empty or all slots are
                    // being inserted concurrently). Break to avoid spinning.
                    break;
                }
                attempts += 1;
                // Re-check whether `slot` was inserted by another thread while we
                // were evicting; if so, Phase 2 will take the Occupied path.
                if self.slots.contains_key(&slot) {
                    break;
                }
            }
        }

        // Phase 2: now acquire the Entry.
        match self.slots.entry(slot) {
            Entry::Occupied(occ) => occ.into_ref(),
            Entry::Vacant(vac) => {
                let r = vac.insert(SlotShreds::new());
                // Increment only after confirmed Vacant insert — no speculative add.
                self.slot_count.fetch_add(1, Ordering::AcqRel);
                metrics::gauge!("nusantara_turbine_tracked_slots")
                    .set(self.slot_count.load(Ordering::Relaxed) as f64);
                r
            }
        }
    }

    /// Decrement `slot_count` by the number of entries actually removed.
    /// Used by callers that call `self.slots.remove()` directly.
    fn account_removed_slots(&self, count: usize) {
        if count > 0 {
            self.slot_count.fetch_sub(count, Ordering::Relaxed);
        }
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

        let mut entry = self.get_or_insert_slot(slot);
        entry.header = Some(header);

        // Retroactively verify all buffered data shreds against the Merkle root.
        let data_evicted = entry.verify_buffered_data_shreds(&merkle_root);
        if data_evicted > 0 {
            metrics::counter!("nusantara_turbine_invalid_shred_signatures")
                .increment(data_evicted as u64);
            tracing::warn!(
                slot,
                evicted = data_evicted,
                "evicted buffered data shreds that failed retroactive merkle verification"
            );
        }

        // Retroactively verify all buffered code shreds against the Merkle root.
        // FEC recovery from verified code shreds is a future improvement.
        let code_evicted = entry.verify_buffered_code_shreds(&merkle_root);
        let code_kept = entry.code_shreds.len();
        if code_evicted > 0 {
            metrics::counter!("nusantara_turbine_invalid_shred_signatures")
                .increment(code_evicted as u64);
            tracing::warn!(
                slot,
                evicted = code_evicted,
                "evicted buffered code shreds that failed retroactive merkle verification"
            );
        }
        if code_kept > 0 {
            tracing::debug!(
                slot,
                code_shreds = code_kept,
                "retroactively verified buffered code shreds (FEC recovery: future work)"
            );
        }

        // Check if the slot is now complete after verification
        if entry.is_complete() {
            let sorted = entry.take_sorted_shreds();
            drop(entry);
            match Deshredder::deshred(&sorted) {
                Ok(block) => {
                    self.stored_slots.insert(slot, ());
                    if self.slots.remove(&slot).is_some() {
                        self.account_removed_slots(1);
                    }
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

        let mut entry = self.get_or_insert_slot(slot);

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
            let sorted = entry.take_sorted_shreds();
            drop(entry);
            match Deshredder::deshred(&sorted) {
                Ok(block) => {
                    // Mark as stored BEFORE removing shred data so duplicate
                    // shreds arriving concurrently are rejected immediately.
                    self.stored_slots.insert(slot, ());
                    if self.slots.remove(&slot).is_some() {
                        self.account_removed_slots(1);
                    }
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

    /// Insert a Merkle code (FEC) shred.
    ///
    /// Code shreds cannot trigger block assembly on their own (that requires all
    /// data shreds). They are buffered so that:
    ///   1. Retroactive Merkle verification can run when the header arrives.
    ///   2. Future FEC recovery work has the shreds available.
    ///
    /// If the batch header has already arrived, the Merkle proof is verified
    /// immediately and the shred is rejected on failure.
    ///
    /// NOTE: FEC recovery from code shreds is not implemented in this pass.
    /// This method ensures code shreds are never silently dropped on the
    /// pre-header path, which previously made FEC recovery structurally impossible.
    pub fn insert_code_shred(&self, shred: &MerkleCodeShred) -> bool {
        let slot = shred.slot();

        if self.stored_slots.contains_key(&slot) {
            metrics::counter!("nusantara_turbine_shreds_skipped_already_stored").increment(1);
            return false;
        }

        let mut entry = self.get_or_insert_slot(slot);

        // If header is present, verify Merkle proof before accepting.
        if let Some(ref header) = entry.header
            && !shred.verify(&header.merkle_root)
        {
            metrics::counter!("nusantara_turbine_invalid_shred_signatures").increment(1);
            return false;
        }

        let accepted = entry.insert_code(shred);
        if accepted {
            metrics::counter!("nusantara_turbine_code_shreds_buffered").increment(1);
        }
        accepted
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

    /// Return the number of buffered code (FEC) shreds for `slot`.
    ///
    /// Useful as production telemetry and in tests that exercise the code-shred
    /// buffer path independently of data-shred assembly.
    pub fn code_shred_count(&self, slot: u64) -> usize {
        self.slots
            .get(&slot)
            .map(|e| e.code_shreds.len())
            .unwrap_or(0)
    }

    pub fn is_slot_complete(&self, slot: u64) -> bool {
        self.slots
            .get(&slot)
            .is_some_and(|e| e.is_complete())
    }

    pub fn remove_slot(&self, slot: u64) {
        if self.slots.remove(&slot).is_some() {
            self.account_removed_slots(1);
        }
    }

    pub fn request_slot_repair(&self, slot: u64) {
        if self.stored_slots.contains_key(&slot) {
            return;
        }
        if self.skip_repair_slots.contains_key(&slot) {
            return;
        }
        // get_or_insert_slot handles cap enforcement and slot_count atomically.
        let _ = self.get_or_insert_slot(slot);
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
        for slot in &old_slots {
            self.slots.remove(slot);
        }
        // Resync atomic counter to true map size after bulk removal.
        let live = self.slots.len();
        self.slot_count.store(live, Ordering::Relaxed);
        metrics::gauge!("nusantara_turbine_tracked_slots").set(live as f64);

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
            metrics::counter!("nusantara_turbine_shred_collector_slots_evicted")
                .increment(count as u64);
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
    use nusantara_crypto::{Hash, Keypair, MerkleTree, hash};

    /// Build a single-leaf code-shred batch with a valid Merkle proof and a
    /// signed header — exercises the code-shred buffer paths without needing
    /// a fat block to trigger FEC.
    fn make_code_batch(kp: &Keypair, slot: u64) -> (ShredBatchHeader, MerkleCodeShred) {
        let code_storage = nusantara_storage::shred::CodeShred {
            slot,
            index: 0,
            num_data_shreds: 1,
            num_code_shreds: 1,
            position: 0,
            data: vec![0xCDu8; 32],
        };
        let mut code = MerkleCodeShred::new(code_storage, kp.address()).unwrap();
        let leaf = code.shred_hash().unwrap();
        let tree = MerkleTree::new(&[leaf]);
        code.merkle_proof = tree.proof(0).unwrap();
        let root = tree.root();
        let header = ShredBatchHeader {
            slot,
            leader: kp.address(),
            merkle_root: root,
            signature: kp.sign(root.as_bytes()),
            num_data_shreds: 0,
            num_code_shreds: 1,
        };
        (header, code)
    }

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
        let merkle = MerkleDataShred::new(shred, kp.address()).unwrap();
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
        let merkle = MerkleDataShred::new(shred, kp.address()).unwrap();
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
        let forged_shred = MerkleDataShred::new(forged_data, kp.address()).unwrap();

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
        let bad_shred = MerkleDataShred::new(bad_data, kp.address()).unwrap();

        // Should be rejected because proof (empty) does not verify
        let result = collector.insert_data_shred(&bad_shred);
        assert!(result.is_none());

        // The bad shred should not be in the collector
        assert!(collector.shred_count(1) == 0 || {
            let entry = collector.slots.get(&1).unwrap();
            !entry.data_shreds.contains_key(&99)
        });
    }

    // -------------------------------------------------------------------------
    // Code-shred buffer path tests (parallel to the data-shred tests above)
    // -------------------------------------------------------------------------

    /// Code shreds arriving before the header must be buffered but must NOT
    /// trigger block assembly (code shreds are FEC parity, not block data).
    #[test]
    fn code_shreds_before_header_waits_for_header() {
        let kp = Keypair::generate();
        let slot = 10u64;
        let (_header, code) = make_code_batch(&kp, slot);
        let collector = ShredCollector::new();

        let accepted = collector.insert_code_shred(&code);
        assert!(accepted, "code shred should be buffered pre-header");

        // Code shred is buffered in the slot entry.
        assert!(collector.has_slot(slot));
        assert_eq!(
            collector.code_shred_count(slot),
            1,
            "code shred must be buffered"
        );

        // No data shreds ⟹ slot is not complete, no block assembled.
        assert!(!collector.is_slot_complete(slot));
        assert_eq!(
            collector.shred_count(slot),
            0,
            "no data shreds should be present"
        );
    }

    /// Forged code shreds that arrive before the header must be evicted during
    /// retroactive Merkle verification when the header arrives.
    #[test]
    fn forged_code_shreds_rejected_after_header_arrival() {
        let kp = Keypair::generate();
        let block = test_block(11);
        let batch = Shredder::shred_block(&block, 10, &kp).unwrap();
        let collector = ShredCollector::new();

        // Forge a code shred with a fresh keypair so its Merkle proof is wrong.
        let forger_kp = Keypair::generate();
        let forged_storage = nusantara_storage::shred::CodeShred {
            slot: batch.header.slot,
            index: 0,
            num_data_shreds: 1,
            num_code_shreds: 1,
            position: 0,
            data: vec![0xDE, 0xAD, 0xBE, 0xEF],
        };
        let forged_code = MerkleCodeShred::new(forged_storage, forger_kp.address()).unwrap();

        // Insert forged code shred before header — buffered without verification.
        let accepted = collector.insert_code_shred(&forged_code);
        assert!(accepted, "forged code shred accepted for buffering pre-header");
        assert_eq!(collector.code_shred_count(batch.header.slot), 1);

        // Insert header — triggers retroactive `verify_buffered_code_shreds`.
        // Forged shred's proof fails against real merkle_root → evicted.
        let result = collector.insert_header(batch.header.clone());
        // Block won't assemble anyway (no data shreds), but the forged code
        // shred must have been evicted during retroactive verification.
        assert!(result.is_none(), "no block — no data shreds were inserted");
        assert_eq!(
            collector.code_shred_count(batch.header.slot),
            0,
            "forged code shred must be evicted after header arrival"
        );
    }

    /// When the header is already present, a code shred with an invalid Merkle
    /// proof is rejected immediately (not buffered).
    #[test]
    fn invalid_code_proof_rejected_with_header_present() {
        let kp = Keypair::generate();
        let block = test_block(12);
        let batch = Shredder::shred_block(&block, 11, &kp).unwrap();
        let collector = ShredCollector::new();

        // Insert header first so Merkle verification is applied inline.
        collector.insert_header(batch.header.clone());
        assert!(collector.has_header(batch.header.slot));

        // Build a code shred signed by a different key — its proof will not
        // verify against the batch's merkle_root.
        let other_kp = Keypair::generate();
        let bad_storage = nusantara_storage::shred::CodeShred {
            slot: batch.header.slot,
            index: 0,
            num_data_shreds: 1,
            num_code_shreds: 1,
            position: 0,
            data: vec![0xFF; 10],
        };
        let bad_code = MerkleCodeShred::new(bad_storage, other_kp.address()).unwrap();

        // Should be rejected — proof cannot verify against the known merkle root.
        let accepted = collector.insert_code_shred(&bad_code);
        assert!(!accepted, "code shred with invalid proof must be rejected");
        assert_eq!(
            collector.code_shred_count(batch.header.slot),
            0,
            "bad code shred must not be buffered"
        );
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
        let merkle = MerkleDataShred::new(shred, kp.address()).unwrap();

        // First insert succeeds (no header, so it gets buffered)
        let r1 = collector.insert_data_shred(&merkle);
        assert!(r1.is_none()); // buffered, not assembled
        assert_eq!(collector.shred_count(1), 1);

        // Second insert of same shred should be skipped (duplicate)
        let r2 = collector.insert_data_shred(&merkle);
        assert!(r2.is_none());
        assert_eq!(collector.shred_count(1), 1); // still 1, not 2
    }

    /// When MAX_TRACKED_SLOTS is exceeded, the oldest slot is evicted.
    #[test]
    fn slot_cap_evicts_oldest() {
        let collector = ShredCollector::new();
        let kp = Keypair::generate();

        // Fill up to cap
        for slot in 0..MAX_TRACKED_SLOTS as u64 {
            let shred = nusantara_storage::shred::DataShred {
                slot,
                index: 0,
                parent_offset: 0,
                data: vec![0u8; 10],
                flags: 0,
            };
            let merkle = MerkleDataShred::new(shred, kp.address()).unwrap();
            collector.insert_data_shred(&merkle);
        }

        assert_eq!(collector.slots.len(), MAX_TRACKED_SLOTS);

        // One more slot should evict slot 0 (oldest)
        let new_slot = MAX_TRACKED_SLOTS as u64;
        let shred = nusantara_storage::shred::DataShred {
            slot: new_slot,
            index: 0,
            parent_offset: 0,
            data: vec![0u8; 10],
            flags: 0,
        };
        let merkle = MerkleDataShred::new(shred, kp.address()).unwrap();
        collector.insert_data_shred(&merkle);

        // Cap is still respected
        assert!(collector.slots.len() <= MAX_TRACKED_SLOTS);
        // New slot was inserted
        assert!(collector.has_slot(new_slot));
    }
}
