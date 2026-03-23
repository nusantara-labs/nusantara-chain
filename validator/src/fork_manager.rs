use std::collections::HashSet;

use tracing::{info, warn};

use crate::constants::{MAX_ROOT_GAP, ORPHAN_HORIZON};
use crate::error::ValidatorError;
use crate::node::ValidatorNode;

impl ValidatorNode {
    /// Advance the fork tree root, but only if doing so won't:
    /// 1. Prune parents that pending orphan blocks depend on
    /// 2. Disconnect fork branches that other validators may be building on
    ///
    /// Without these gates, root advancement prunes other validators' forks,
    /// causing permanent chain divergence.
    pub(crate) fn try_advance_root(
        &mut self,
        proposed_root: u64,
    ) -> Result<(), ValidatorError> {
        let current_root = self.replay_stage.fork_tree().root_slot();
        if proposed_root <= current_root {
            return Ok(());
        }

        // Safety valve: if the gap between proposed and current root exceeds
        // MAX_ROOT_GAP, force advance bypassing both gates.
        if proposed_root > current_root + MAX_ROOT_GAP {
            tracing::warn!(
                proposed_root,
                current_root,
                gap = proposed_root - current_root,
                orphan_count = self.orphan_blocks.len(),
                "forcing root advancement — gap exceeds {MAX_ROOT_GAP} slots"
            );
            metrics::counter!("nusantara_root_safety_valve_activated").increment(1);
            self.replay_stage.advance_root(proposed_root)?;
            metrics::gauge!("nusantara_root_slot").set(proposed_root as f64);
            self.failed_fork_targets.clear();
            return Ok(());
        }

        let mut safe_root = proposed_root;

        // Gate 1: Don't prune parents of *recent* orphan blocks.
        if !self.orphan_blocks.is_empty() {
            let horizon = proposed_root.saturating_sub(ORPHAN_HORIZON);
            let min_recent_orphan_parent = self
                .orphan_blocks
                .values()
                .map(|b| b.header.parent_slot)
                .filter(|&p| p >= horizon)
                .min();
            if let Some(min_parent) = min_recent_orphan_parent {
                safe_root = safe_root.min(min_parent.saturating_sub(1));
            }
        }

        // Gate 2: Don't advance past *recent* fork points that have branches
        // outside the proposed root's ancestry chain.
        let fork_horizon = proposed_root.saturating_sub(ORPHAN_HORIZON);
        let ancestry = self.replay_stage.fork_tree().get_ancestry(proposed_root);
        let ancestry_set: HashSet<u64> = ancestry.iter().copied().collect();
        for &slot in &ancestry {
            if slot < current_root || slot < fork_horizon {
                break;
            }
            if let Some(node) = self.replay_stage.fork_tree().get_node(slot) {
                let has_branch = node
                    .children
                    .iter()
                    .any(|child| !ancestry_set.contains(child));
                if has_branch {
                    safe_root = safe_root.min(slot);
                    tracing::debug!(
                        fork_point = slot,
                        proposed_root,
                        "limiting root to preserve fork branch"
                    );
                }
            }
        }

        if safe_root > current_root {
            self.replay_stage.advance_root(safe_root)?;
            metrics::gauge!("nusantara_root_slot").set(safe_root as f64);
            self.failed_fork_targets.clear();
            if safe_root < proposed_root {
                tracing::debug!(
                    proposed_root,
                    safe_root,
                    orphan_count = self.orphan_blocks.len(),
                    "root advancement limited to preserve forks/orphans"
                );
            }
        } else {
            if proposed_root > current_root + 10 {
                tracing::debug!(
                    proposed_root,
                    current_root,
                    orphan_count = self.orphan_blocks.len(),
                    "root advancement suppressed — preserving forks/orphan parents"
                );
            }
            metrics::counter!("nusantara_root_advancement_deferred").increment(1);
        }
        Ok(())
    }

    /// Handle a fork switch by rolling back to common ancestor and replaying.
    ///
    /// On failure, restores bank slot_hashes and records the target in
    /// `failed_fork_targets` to prevent infinite retry.
    pub(crate) fn handle_fork_switch(
        &mut self,
        plan: nusantara_consensus::replay_stage::ForkSwitchPlan,
    ) {
        let target = plan
            .replay_slots
            .last()
            .copied()
            .unwrap_or(plan.common_ancestor);

        info!(
            common_ancestor = plan.common_ancestor,
            rollback_from = plan.rollback_from,
            replay_count = plan.replay_slots.len(),
            target,
            "switching forks"
        );

        // 1. Rollback bank to common ancestor
        if let Err(e) = self.bank.rollback_to_slot(plan.common_ancestor, &self.storage) {
            warn!(error = %e, "fork switch: bank rollback failed");
            self.failed_fork_targets.insert(target);
            return;
        }

        // 2. Rewind account index
        match self.storage.rewind_account_index_to_slot(plan.common_ancestor) {
            Ok(rewound) => {
                if rewound > 0 {
                    info!(rewound, "account index rewound for fork switch");
                }
            }
            Err(e) => {
                warn!(error = %e, "fork switch: account index rewind failed");
                self.restore_bank_slot_hashes();
                self.failed_fork_targets.insert(target);
                return;
            }
        }

        // 3. Replay blocks on the new fork (skip slots already in fork tree)
        for slot in &plan.replay_slots {
            if self.replay_stage.fork_tree().contains(*slot) {
                tracing::debug!(slot, "slot already in fork tree, skipping fork-switch replay");
                continue;
            }
            match self.storage.get_block(*slot) {
                Ok(Some(block)) => {
                    if let Err(e) = crate::block_replayer::replay_block_full(
                        &block,
                        &self.storage,
                        &self.bank,
                        &mut self.replay_stage,
                        &self.fee_calculator,
                        &self.rent,
                        &self.epoch_schedule,
                        &self.program_cache,
                    ) {
                        warn!(
                            slot,
                            error = %e,
                            "fork switch replay failed — aborting switch"
                        );
                        self.restore_bank_slot_hashes();
                        self.failed_fork_targets.insert(target);
                        metrics::counter!("nusantara_fork_switch_failures").increment(1);
                        return;
                    }
                }
                Ok(None) => {
                    warn!(slot, "block not found for fork replay — aborting switch");
                    self.restore_bank_slot_hashes();
                    self.failed_fork_targets.insert(target);
                    return;
                }
                Err(e) => {
                    warn!(slot, error = %e, "failed to load block for fork replay");
                    self.restore_bank_slot_hashes();
                    self.failed_fork_targets.insert(target);
                    return;
                }
            }
        }

        // 4. Update block producer parent to new fork tip
        if let Some(node) = self.replay_stage.fork_tree().get_node(target) {
            self.block_producer
                .set_parent(target, node.block_hash, node.bank_hash);
        }

        // Success — clear failed targets since fork landscape changed
        self.failed_fork_targets.clear();
        info!(new_tip = target, "fork switch complete");
        metrics::counter!("nusantara_fork_switches_completed").increment(1);
    }
}
