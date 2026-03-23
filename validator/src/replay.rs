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
        let merged =
            crate::helpers::build_merged_slot_hashes(&entries, &self.storage, 512);
        self.bank.set_slot_hashes(SlotHashes(merged));
    }

    /// Replay a received block or buffer it if parent is missing.
    /// On verification mismatch, rewinds storage and discards the block.
    pub(crate) fn replay_or_buffer_block(
        &mut self,
        block: Block,
    ) -> Result<(), ValidatorError> {
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
                self.block_producer.set_parent(
                    slot,
                    block.header.block_hash,
                    result.bank_hash,
                );

                // Defer root advancement
                if let Some(root) = result.new_root {
                    self.try_advance_root(root)?;
                }

                self.consecutive_skips.store(0, Ordering::Relaxed);

                // Publish pubsub events
                let root = self
                    .storage
                    .get_latest_root()
                    .unwrap_or(None)
                    .unwrap_or(0);
                if let Err(e) = self.pubsub_tx.send(PubsubEvent::SlotUpdate {
                    slot,
                    parent: parent_slot,
                    root,
                }) {
                    tracing::debug!(error = %e, "pubsub SlotUpdate send failed (no subscribers)");
                }
                if let Err(e) = self.pubsub_tx.send(PubsubEvent::BlockNotification {
                    slot,
                    block_hash: block.header.block_hash.to_base64(),
                    tx_count: block.header.transaction_count,
                }) {
                    tracing::debug!(error = %e, "pubsub BlockNotification send failed (no subscribers)");
                }

                // Publish SignatureNotification for each transaction in the block
                for tx in &block.transactions {
                    let tx_hash = tx.hash();
                    let sig_b64 = tx_hash.to_base64();
                    let status_str = match self.storage.get_transaction_status(&tx_hash) {
                        Ok(Some(meta)) => match &meta.status {
                            nusantara_storage::TransactionStatus::Success => {
                                "success".to_string()
                            }
                            nusantara_storage::TransactionStatus::Failed(msg) => {
                                format!("failed: {msg}")
                            }
                        },
                        _ => "success".to_string(),
                    };
                    let _ = self.pubsub_tx.send(PubsubEvent::SignatureNotification {
                        signature: sig_b64,
                        slot,
                        status: status_str,
                    });
                }

                metrics::counter!("nusantara_blocks_replayed").increment(1);
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
                        parent_slot,
                        root,
                        "discarding block — parent already finalized and pruned"
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
                // Evict oldest orphans if buffer is full
                while self.orphan_blocks.len() >= MAX_ORPHAN_BUFFER_SIZE {
                    if let Some(oldest_slot) = self.orphan_blocks.keys().next().copied() {
                        self.orphan_blocks.remove(&oldest_slot);
                        metrics::counter!("nusantara_orphan_evictions").increment(1);
                    }
                }
                self.orphan_blocks.insert(slot, block);
                self.request_missing_slots(parent_slot);
                metrics::counter!("nusantara_orphan_blocks_buffered").increment(1);
                metrics::gauge!("nusantara_orphan_queue_size")
                    .set(self.orphan_blocks.len() as f64);
                Ok(())
            }
            Err(ValidatorError::BankHashMismatch { slot })
            | Err(ValidatorError::MerkleRootMismatch { slot })
            | Err(ValidatorError::BlockHashMismatch { slot }) => {
                warn!(slot, "block verification mismatch — discarding");
                if !already_stored {
                    let _ = self.storage.delete_block(slot);
                    metrics::counter!("nusantara_blocks_deleted_verification_failure")
                        .increment(1);
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
        let cutoff = self.current_slot.saturating_sub(ORPHAN_HORIZON);
        self.orphan_blocks.retain(|slot, _| *slot > cutoff);

        let root = self.replay_stage.fork_tree().root_slot();
        let before = self.orphan_blocks.len();
        self.orphan_blocks
            .retain(|_slot, block| block.header.parent_slot >= root);
        let pruned = before - self.orphan_blocks.len();
        if pruned > 0 {
            debug!(
                pruned_count = pruned,
                root,
                remaining = self.orphan_blocks.len(),
                "discarded irrecoverable orphans (parent below root)"
            );
            metrics::counter!("nusantara_orphan_blocks_pruned_below_root")
                .increment(pruned as u64);
            metrics::gauge!("nusantara_orphan_queue_size")
                .set(self.orphan_blocks.len() as f64);
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
            let block = self.orphan_blocks.remove(&slot).unwrap();

            info!(slot, parent_slot = block.header.parent_slot, "replaying buffered orphan block");

            self.replay_or_buffer_block(block)?;

            metrics::counter!("nusantara_orphan_blocks_replayed").increment(1);
            metrics::gauge!("nusantara_orphan_queue_size")
                .set(self.orphan_blocks.len() as f64);
        }
        Ok(())
    }

    /// Request repair for missing ancestors across ALL orphan chains.
    pub(crate) fn request_missing_slots(&self, _needed_slot: u64) {
        let root = self.replay_stage.fork_tree().root_slot();
        let mut to_repair = Vec::new();

        for block in self.orphan_blocks.values() {
            let parent = block.header.parent_slot;
            if parent >= root
                && self.replay_stage.fork_tree().get_node(parent).is_none()
                && !self.orphan_blocks.contains_key(&parent)
            {
                to_repair.push(parent);
            }
        }

        to_repair.sort_unstable();
        to_repair.dedup();
        let count = to_repair.len().min(32);
        for &slot in &to_repair[..count] {
            self.shred_collector.request_slot_repair(slot);
        }

        if count > 0 {
            debug!(
                gap_roots = count,
                fork_tree_root = root,
                orphan_count = self.orphan_blocks.len(),
                "requesting repair for missing ancestor slots"
            );
        }
    }
}
