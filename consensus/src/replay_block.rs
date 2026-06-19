use nusantara_core::block::Block;
use nusantara_crypto::Hash;
use tracing::instrument;

use crate::bank::FrozenBankState;
use crate::error::ConsensusError;
use crate::poh::{PohEntry, verify_poh_entries};
use crate::replay_stage::{ReplayResult, ReplayStage};
use crate::replay_vote_processing::extract_votes_from_transaction;

impl ReplayStage {
    /// Replay a block through the consensus pipeline.
    ///
    /// `parent_poh` is the PoH hash at the end of the parent slot — used as the
    /// initial hash for verifying the first entry in `poh_entries` (B3).
    #[instrument(skip(self, block, poh_entries, parent_poh), fields(slot = block.header.slot), level = "info")]
    pub fn replay_block(
        &mut self,
        block: &Block,
        poh_entries: &[PohEntry],
        parent_poh: &Hash,
    ) -> Result<ReplayResult, ConsensusError> {
        let slot = block.header.slot;
        let parent_slot = block.header.parent_slot;
        let block_hash = block.header.block_hash;

        tracing::debug!(slot, parent_slot, "Replaying block");

        // 1. Verify leader matches schedule (if schedule is cached)
        let epoch = self.bank.epoch_schedule().get_epoch(slot);
        if let Some(schedule) = self.leader_schedule_cache.get(&epoch)
            && let Some(expected_leader) = schedule.get_leader(slot, self.bank.epoch_schedule())
            && *expected_leader != block.header.validator
        {
            return Err(ConsensusError::WrongLeader {
                slot,
                expected: expected_leader.to_base58(),
                got: block.header.validator.to_base58(),
            });
        }

        // 2. Verify PoH entries — verify from parent_poh through the full slice (B3).
        // Entry 0 is verified against parent_poh (previously skipped with [1..]).
        if !poh_entries.is_empty() {
            let poh_valid = if let Some(ref gpu) = self.gpu_verifier {
                // Build GPU windows: (initial_hash, delta, expected_hash).
                // The first window uses parent_poh as the initial hash (B3).
                let mut gpu_entries: Vec<(Hash, u64, Hash)> =
                    Vec::with_capacity(poh_entries.len());

                // First entry: parent_poh -> poh_entries[0]
                let first_delta = poh_entries[0].num_hashes;
                gpu_entries.push((*parent_poh, first_delta, poh_entries[0].hash));

                // Remaining entries: poh_entries[i] -> poh_entries[i+1]
                for w in poh_entries.windows(2) {
                    let delta = w[1]
                        .num_hashes
                        .checked_sub(w[0].num_hashes)
                        .ok_or(ConsensusError::PohVerificationFailed { index: 0 })?;
                    gpu_entries.push((w[0].hash, delta, w[1].hash));
                }

                if gpu_entries.iter().any(|(_, delta, _)| *delta > u32::MAX as u64) {
                    tracing::warn!("PoH delta exceeds u32::MAX, falling back to CPU");
                    // B5: block_in_place keeps the async caller from blocking the executor.
                    let result = verify_poh_entries(parent_poh, poh_entries);
                    result.is_ok()
                } else {
                    // B5: GPU polling is blocking — use block_in_place so tokio can park
                    // the thread instead of blocking the runtime's thread pool.
                    let gpu_result = tokio::task::block_in_place(|| gpu.verify_batch(&gpu_entries));
                    match gpu_result {
                        Ok(results) => results.iter().all(|&r| r),
                        Err(_) => {
                            tracing::warn!("GPU verification failed, falling back to CPU");
                            verify_poh_entries(parent_poh, poh_entries).is_ok()
                        }
                    }
                }
            } else {
                verify_poh_entries(parent_poh, poh_entries).is_ok()
            };

            if !poh_valid {
                return Err(ConsensusError::PohVerificationFailed { index: 0 });
            }
        }

        // 3. Add slot to fork tree
        // Use the block header's bank_hash directly. For leader-produced blocks,
        // BlockProducer computed it from real account deltas. For observer-replayed
        // blocks, replay_block_full() verified it via re-execution.
        let frozen = FrozenBankState {
            slot,
            parent_slot,
            block_hash,
            bank_hash: block.header.bank_hash,
            epoch: self.bank.epoch_schedule().get_epoch(slot),
            transaction_count: block.header.transaction_count,
        };

        self.fork_tree
            .add_slot(slot, parent_slot, block_hash, frozen.bank_hash)?;

        // 3b. Drain any gossip votes that arrived before this slot was in the tree.
        self.drain_pending_votes(slot);

        // 4. Extract vote transactions and process
        //
        // Only our own votes go through the local Tower (lockout enforcement).
        // Other validators' votes only update fork tree weights and commitment.
        let mut vote_count = 0u64;
        let mut new_root = None;
        let root_slot = self.tower.root_slot().unwrap_or(0);

        for tx in &block.transactions {
            // B11: extract_votes_from_transaction returns all votes in the tx.
            for (voter, vote) in extract_votes_from_transaction(tx) {
                let highest_vote_slot = vote.slots.last().copied().unwrap_or(0);
                if highest_vote_slot <= root_slot {
                    continue;
                }

                let stake = self.bank.get_validator_stake(&voter);
                let is_own_vote = voter == self.authorized_voter;

                if is_own_vote {
                    // Process through tower (lockout enforcement)
                    match self.tower.process_vote(&vote) {
                        Ok(result) => {
                            if let Some(&voted_slot) = vote.slots.last() {
                                // B1: pass voter identity to deduplicate stake.
                                self.fork_tree.add_vote(voted_slot, voter, stake);
                                let voted_block_hash = self
                                    .fork_tree
                                    .get_node(voted_slot)
                                    .map(|n| n.block_hash)
                                    .unwrap_or(vote.hash);
                                self.commitment_tracker.record_vote(
                                    voted_slot,
                                    voted_block_hash,
                                    voter,
                                    stake,
                                );
                            }
                            // Mark ALL intermediate roots as finalized, not just the last
                            for &root in &result.new_root_slots {
                                new_root = Some(root);
                                self.commitment_tracker.mark_finalized(root);
                            }
                            vote_count += 1;
                        }
                        Err(e) => {
                            tracing::debug!(?e, "Own vote processing failed, skipping");
                        }
                    }
                } else {
                    // Other validator's vote — update fork weights only, no lockout check
                    if let Some(&voted_slot) = vote.slots.last() {
                        // B1: pass voter identity to deduplicate stake.
                        self.fork_tree.add_vote(voted_slot, voter, stake);
                        let voted_block_hash = self
                            .fork_tree
                            .get_node(voted_slot)
                            .map(|n| n.block_hash)
                            .unwrap_or(vote.hash);
                        self.commitment_tracker
                            .record_vote(voted_slot, voted_block_hash, voter, stake);
                    }
                    vote_count += 1;
                }
            }
        }

        // 5. Advance bank slot
        self.bank.advance_slot(slot, block.header.timestamp);
        self.bank.record_slot_hash(slot, block_hash);

        // 6. Root advancement is deferred to the caller via `advance_root()`.
        //
        // Previously this was done inline, but the caller (ValidatorNode) needs
        // to gate root advancement on whether orphan blocks would be pruned.
        // Premature root advancement permanently prevents replay of cross-
        // validator blocks whose parents have been pruned from the fork tree.

        // 7. Compute best fork
        self.fork_tree.compute_best_fork();

        // 8. Freeze bank -> persist
        self.bank.flush_to_storage(&frozen)?;

        // Track commitment for this slot
        self.commitment_tracker.track_slot(slot, block_hash);

        // Update current tip
        self.current_tip = slot;

        metrics::counter!("nusantara_replay_blocks_processed_total").increment(1);
        metrics::counter!("nusantara_replay_votes_processed_total").increment(vote_count);

        Ok(ReplayResult {
            slot,
            block_hash,
            bank_hash: frozen.bank_hash,
            parent_slot,
            transaction_count: block.header.transaction_count,
            vote_count,
            new_root,
        })
    }
}

#[cfg(test)]
mod tests {
    use nusantara_crypto::Hash;

    use crate::test_utils::test_helpers::{make_block, make_replay_stage};

    #[test]
    fn replay_empty_block() {
        let (mut stage, _dir) = make_replay_stage();
        let block = make_block(1, 0);
        let result = stage.replay_block(&block, &[], &Hash::zero()).unwrap();
        assert_eq!(result.slot, 1);
        assert_eq!(result.vote_count, 0);
        assert!(result.new_root.is_none());
    }

    #[test]
    fn replay_sequential_blocks() {
        let (mut stage, _dir) = make_replay_stage();
        for slot in 1..=5 {
            let block = make_block(slot, slot - 1);
            let result = stage.replay_block(&block, &[], &Hash::zero()).unwrap();
            assert_eq!(result.slot, slot);
        }
        assert_eq!(stage.fork_tree().node_count(), 6); // root + 5 blocks
    }

    #[test]
    fn replay_fork() {
        let (mut stage, _dir) = make_replay_stage();
        // Linear: 0 -> 1 -> 2
        stage.replay_block(&make_block(1, 0), &[], &Hash::zero()).unwrap();
        stage.replay_block(&make_block(2, 1), &[], &Hash::zero()).unwrap();
        // Fork: 0 -> 3
        stage.replay_block(&make_block(3, 0), &[], &Hash::zero()).unwrap();

        assert_eq!(stage.fork_tree().node_count(), 4);
    }

    #[test]
    fn replay_duplicate_slot_fails() {
        let (mut stage, _dir) = make_replay_stage();
        stage.replay_block(&make_block(1, 0), &[], &Hash::zero()).unwrap();
        let result = stage.replay_block(&make_block(1, 0), &[], &Hash::zero());
        assert!(result.is_err());
    }
}
