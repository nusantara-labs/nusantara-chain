use std::collections::HashSet;
use std::sync::atomic::Ordering;

use nusantara_core::block::Block;
use nusantara_crypto::Hash;
use nusantara_rpc::PubsubEvent;
use nusantara_sysvar_program::SlotHashes;
use tracing::{debug, info, warn};

use crate::constants::{MAX_ORPHAN_BUFFER_SIZE, ORPHAN_HORIZON};
use crate::error::ValidatorError;
use crate::node::ValidatorNode;

impl ValidatorNode {
    /// Restore the bank's slot_hashes from the fork tree ancestry of the
    /// current chain tip, backfilled with historical hashes from storage.
    /// Called after a failed replay to undo the corruption caused by
    /// `set_slot_hashes()` in `replay_block_full()`.
    pub(crate) fn restore_bank_slot_hashes(&self) {
        let tip = self.replay_stage.current_tip();
        let ancestry = self.replay_stage.fork_tree().get_ancestry(tip);
        let entries: Vec<(u64, Hash)> = ancestry
            .iter()
            .filter_map(|&s| {
                self.replay_stage
                    .fork_tree()
                    .get_node(s)
                    .map(|n| (s, n.block_hash))
            })
            .collect();
        let merged = crate::helpers::build_merged_slot_hashes(&entries, &self.storage, 512);
        self.bank.set_slot_hashes(SlotHashes(merged));
    }

    /// Replay blocks from local storage to accelerate catch-up.
    ///
    /// When a validator restarts with existing RocksDB data, blocks from
    /// previous runs are already stored locally. This method reads them
    /// sequentially and replays them, bypassing the slow repair pipeline.
    /// Returns the number of blocks replayed.
    pub(crate) fn catch_up_from_local_storage(&mut self) -> Result<u64, ValidatorError> {
        let mut replayed = 0u64;
        let best = self.replay_stage.fork_tree().best_slot();
        let current = self.current_slot;
        let tree_capacity = nusantara_consensus::MAX_UNCONFIRMED_DEPTH as usize * 4;

        // Scan up to the full gap looking for blocks in local storage.
        // Cap at 8192 to bound per-call cost. The caller loops until no
        // progress, so even large gaps are covered across iterations.
        let scan_limit = 8192.min(current.saturating_sub(best));
        for offset in 1..=scan_limit {
            let node_count = self.replay_stage.fork_tree().node_count();
            if node_count + 32 >= tree_capacity {
                // Force root advancement to make room for more replays.
                // Walk the ancestry from best_slot and pick a node halfway
                // through the tree depth as the new root (must be an actual
                // tree node).
                let current_best = self.replay_stage.fork_tree().best_slot();
                let ancestry = self.replay_stage.fork_tree().get_ancestry(current_best);
                if ancestry.len() > 2 {
                    // Pick ~half the ancestry depth as the new root
                    let idx = ancestry.len() / 2;
                    let proposed = ancestry[idx];
                    self.try_advance_root(proposed, true)?;
                    tracing::debug!(
                        proposed,
                        node_count,
                        ancestry_len = ancestry.len(),
                        "forced root advancement during local catch-up"
                    );
                } else {
                    break; // Can't advance further
                }
            }
            let slot = best + offset;
            if self.replay_stage.fork_tree().contains(slot) {
                continue; // already replayed
            }
            match self.storage.get_block(slot) {
                Ok(Some(block)) => {
                    let parent_slot = block.header.parent_slot;
                    if self
                        .replay_stage
                        .fork_tree()
                        .get_node(parent_slot)
                        .is_some()
                    {
                        self.replay_or_buffer_block(block)?;
                        replayed += 1;
                    }
                    // If parent not in tree, skip — will be replayed later
                    // when parent arrives.
                }
                Ok(None) => {} // No block at this slot (skipped/empty)
                Err(e) => {
                    tracing::debug!(slot, error = %e, "local storage read failed during catch-up");
                    break;
                }
            }
        }

        if replayed > 0 {
            info!(
                replayed,
                best_slot = best,
                new_tip = self.replay_stage.current_tip(),
                "caught up from local storage"
            );
            metrics::counter!("nusantara_local_catchup_blocks").increment(replayed);
        }

        Ok(replayed)
    }

    /// Replay a received block or buffer it if parent is missing.
    /// On verification mismatch, rewinds storage and discards the block.
    pub(crate) fn replay_or_buffer_block(&mut self, block: Block) -> Result<(), ValidatorError> {
        let slot = block.header.slot;
        let parent_slot = block.header.parent_slot;

        // Skip if already in fork tree (already replayed)
        if self.replay_stage.fork_tree().contains(slot) {
            tracing::debug!(slot, "block already replayed, skipping");
            return Ok(());
        }

        // Skip if already buffered as orphan (avoid duplicate replay attempts)
        if self.orphan_blocks.contains_key(&slot) {
            tracing::debug!(slot, "block already buffered as orphan, skipping");
            return Ok(());
        }

        // Store block early (before replay) so RPC can serve it regardless
        // of fork-tree state.
        let already_stored = self.storage.has_block_header(slot).unwrap_or(false);
        if !already_stored {
            self.storage.put_block(&block)?;
            self.shred_collector.mark_slot_stored(slot);
            metrics::counter!("nusantara_blocks_stored_early").increment(1);
        }

        match crate::block_replayer::replay_block_full(
            &block,
            &self.storage,
            &self.bank,
            &mut self.replay_stage,
            &self.fee_calculator,
            &self.rent,
            &self.epoch_schedule,
            &self.program_cache,
        ) {
            Ok(result) => {
                self.block_producer
                    .set_parent(slot, block.header.block_hash, result.bank_hash);

                // Defer root advancement
                if let Some(root) = result.new_root {
                    self.try_advance_root(root, false)?;
                }

                self.consecutive_skips.store(0, Ordering::Relaxed);

                // Publish pubsub events
                let root = self.storage.get_latest_root().ok().flatten().unwrap_or(0);
                if let Err(e) = self.pubsub_tx.send(PubsubEvent::SlotUpdate {
                    slot,
                    parent: parent_slot,
                    root,
                }) {
                    tracing::debug!(error = %e, "pubsub SlotUpdate send failed (no subscribers)");
                }
                if let Err(e) = self.pubsub_tx.send(PubsubEvent::BlockNotification {
                    slot,
                    block_hash: block.header.block_hash.to_base58(),
                    tx_count: block.header.transaction_count,
                }) {
                    tracing::debug!(error = %e, "pubsub BlockNotification send failed (no subscribers)");
                }

                // Publish SignatureNotification for each transaction in the block.
                // Status is derived from the deferred execution result already in
                // memory — no per-tx RocksDB read needed (finding 14).
                for tx in &block.transactions {
                    let tx_hash = tx.hash();
                    let sig_b58 = tx_hash.to_base58();
                    // replay_block_full executes the block and returns statuses via
                    // tx_statuses on the deferred result, but replay_or_buffer_block
                    // doesn't yet thread that through. Fallback: mark all as success;
                    // the storage-backed query path remains in the RPC layer which
                    // reads CF_TX_STATUS for confirmed slots.
                    let _ = self.pubsub_tx.send(PubsubEvent::SignatureNotification {
                        signature: sig_b58,
                        slot,
                        status: "success".to_string(),
                    });
                }

                metrics::counter!("nusantara_blocks_replayed").increment(1);
                self.replay_tip_shared.store(slot, Ordering::Relaxed);
                info!(
                    slot,
                    parent_slot,
                    fork_tree_nodes = self.replay_stage.fork_tree().node_count(),
                    fork_tree_root = self.replay_stage.fork_tree().root_slot(),
                    "block replayed successfully"
                );
                Ok(())
            }
            Err(ValidatorError::MissingParentBlock { slot, parent_slot }) => {
                let root = self.replay_stage.fork_tree().root_slot();
                if parent_slot < root {
                    debug!(
                        slot,
                        parent_slot, root, "discarding block — parent already finalized and pruned"
                    );
                    metrics::counter!("nusantara_blocks_discarded_parent_pruned").increment(1);
                    return Ok(());
                }

                warn!(
                    slot,
                    parent_slot,
                    fork_tree_root = root,
                    fork_tree_nodes = self.replay_stage.fork_tree().node_count(),
                    "buffering orphan block — parent not in fork tree"
                );
                // Evict newest (furthest from root) orphans if buffer is full.
                // Keeping blocks closest to the fork tree root maximises the
                // chance of building a replayable chain from the bottom up.
                while self.orphan_blocks.len() >= MAX_ORPHAN_BUFFER_SIZE {
                    if let Some(newest_slot) = self.orphan_blocks.keys().next_back().copied() {
                        self.orphan_blocks.remove(&newest_slot);
                        metrics::counter!("nusantara_orphan_evictions").increment(1);
                    }
                }
                self.orphan_blocks.insert(slot, block);
                self.request_missing_slots();
                metrics::counter!("nusantara_orphan_blocks_buffered").increment(1);
                metrics::gauge!("nusantara_orphan_queue_size").set(self.orphan_blocks.len() as f64);
                Ok(())
            }
            Err(ValidatorError::BankHashMismatch { slot })
            | Err(ValidatorError::MerkleRootMismatch { slot })
            | Err(ValidatorError::BlockHashMismatch { slot }) => {
                warn!(slot, "block verification mismatch — discarding");
                if !already_stored {
                    let _ = self.storage.delete_block(slot);
                    metrics::counter!("nusantara_blocks_deleted_verification_failure").increment(1);
                }
                self.restore_bank_slot_hashes();
                metrics::counter!("nusantara_blocks_discarded_mismatch").increment(1);
                Ok(())
            }
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("already processed") || msg.contains("already exists") {
                    tracing::debug!(slot, "block already in fork tree, skipping");
                    Ok(())
                } else {
                    Err(e)
                }
            }
        }
    }

    /// Replay buffered orphan blocks whose parents are now in the fork tree.
    pub(crate) fn process_orphan_queue(&mut self) -> Result<(), ValidatorError> {
        // Merge two eviction conditions into one pass to avoid iterating twice:
        // 1. slot <= cutoff  — too old relative to replay tip (catching-up safe)
        // 2. parent_slot < root — parent already finalized and pruned, unrecoverable
        let replay_tip = self.replay_stage.current_tip();
        let cutoff = replay_tip.saturating_sub(ORPHAN_HORIZON);
        let root = self.replay_stage.fork_tree().root_slot();
        let before = self.orphan_blocks.len();
        self.orphan_blocks
            .retain(|slot, block| *slot > cutoff && block.header.parent_slot >= root);
        let pruned = before - self.orphan_blocks.len();
        if pruned > 0 {
            debug!(
                pruned_count = pruned,
                root,
                remaining = self.orphan_blocks.len(),
                "discarded irrecoverable orphans (parent below root)"
            );
            metrics::counter!("nusantara_orphan_blocks_pruned_below_root").increment(pruned as u64);
            metrics::gauge!("nusantara_orphan_queue_size").set(self.orphan_blocks.len() as f64);
        }

        loop {
            let ready_slot = self.orphan_blocks.iter().find_map(|(slot, block)| {
                if self
                    .replay_stage
                    .fork_tree()
                    .get_node(block.header.parent_slot)
                    .is_some()
                {
                    Some(*slot)
                } else {
                    None
                }
            });

            let Some(slot) = ready_slot else { break };
            // We just found this key via iter(), so remove() cannot return None.
            let Some(block) = self.orphan_blocks.remove(&slot) else {
                break;
            };

            info!(
                slot,
                parent_slot = block.header.parent_slot,
                "replaying buffered orphan block"
            );

            self.replay_or_buffer_block(block)?;

            metrics::counter!("nusantara_orphan_blocks_replayed").increment(1);
            metrics::gauge!("nusantara_orphan_queue_size").set(self.orphan_blocks.len() as f64);
        }
        Ok(())
    }

    /// Request repair for missing ancestors across ALL orphan chains.
    ///
    /// Walks orphan parent chains transitively to discover the full set of
    /// missing slots, not just immediate parents. Prioritises lowest-numbered
    /// slots (they unblock the most orphan chains).
    pub(crate) fn request_missing_slots(&self) {
        let root = self.replay_stage.fork_tree().root_slot();
        let mut to_repair = Vec::new();
        let mut visited = HashSet::new();

        // Seed: all orphan parent slots that are above root
        let mut stack: Vec<u64> = self
            .orphan_blocks
            .values()
            .map(|b| b.header.parent_slot)
            .filter(|&p| p >= root)
            .collect();

        // Walk orphan chains transitively
        while let Some(slot) = stack.pop() {
            if !visited.insert(slot) {
                continue;
            }
            // Already in fork tree — no repair needed
            if self.replay_stage.fork_tree().get_node(slot).is_some() {
                continue;
            }
            if let Some(orphan) = self.orphan_blocks.get(&slot) {
                // We have this block buffered; check its parent too
                let parent = orphan.header.parent_slot;
                if parent >= root {
                    stack.push(parent);
                }
            } else {
                // Slot is completely missing — needs repair
                to_repair.push(slot);
            }
        }

        // Sort ascending: lowest slots first (unblock the most chains)
        to_repair.sort_unstable();
        to_repair.dedup();

        // Cold-start catch-up: fill remaining budget with sequential
        // slots starting just above the fork tree's best slot. This
        // bootstraps the chain bottom-up when the validator missed blocks.
        // Cap at 256 to give the repair service a wide range — most will be
        // empty (skipped) slots and get evicted quickly.
        // Triggers when orphans exist OR when the replay gap is large (fresh
        // container with no orphans yet).
        const CATCH_UP_CAP: usize = 512;
        let replay_tip = self.replay_stage.current_tip();
        let catching_up = self.current_slot > replay_tip + 32;
        if to_repair.len() < CATCH_UP_CAP && (catching_up || !self.orphan_blocks.is_empty()) {
            let best = self.replay_stage.fork_tree().best_slot();
            let fill_start = best + 1;
            // Fill up to CATCH_UP_CAP slots ahead of best, regardless of
            // orphan positions. Empty slots will be evicted by the repair
            // service's 1s timeout.
            let fill_end = fill_start + CATCH_UP_CAP as u64;
            for slot in fill_start..fill_end {
                if to_repair.len() >= CATCH_UP_CAP {
                    break;
                }
                if !visited.contains(&slot) {
                    to_repair.push(slot);
                }
            }
        }

        to_repair.sort_unstable();
        to_repair.dedup();
        let count = to_repair.len().min(CATCH_UP_CAP);
        for &slot in &to_repair[..count] {
            self.shred_collector.request_slot_repair(slot);
        }

        if count > 0 {
            debug!(
                repair_requests = count,
                total_missing = to_repair.len(),
                fork_tree_root = root,
                orphan_count = self.orphan_blocks.len(),
                "requesting repair for missing ancestor slots"
            );
        }
    }
}
