use std::collections::HashSet;

use crate::error::ConsensusError;
use crate::replay_stage::{ForkSwitchPlan, ReplayStage};

impl ReplayStage {
    /// Advance the fork tree root to the given slot.
    ///
    /// This prunes all fork tree nodes below `root` and marks the slot as
    /// finalized in storage. The caller is responsible for deciding WHEN to
    /// advance — typically gated on whether pending orphan blocks would lose
    /// their parents.
    pub fn advance_root(&mut self, root: u64) -> Result<(), ConsensusError> {
        if !self.fork_tree.contains(root) {
            tracing::debug!(root, "skipping root advancement — slot not in fork tree");
            return Ok(());
        }
        let pruned = self.fork_tree.set_root(root);
        self.commitment_tracker.prune_below(root);
        self.bank.set_root(root)?;
        tracing::info!(root, pruned_count = pruned.len(), "Root advanced");
        Ok(())
    }

    /// Check if we should switch to a different fork.
    ///
    /// Returns `Some(ForkSwitchPlan)` if the best fork diverges from our current tip
    /// and Tower lockout rules allow switching.
    pub fn check_fork_switch(&self) -> Option<ForkSwitchPlan> {
        let best = self.fork_tree.best_slot();
        if best == self.current_tip {
            return None;
        }

        let best_ancestry = self.fork_tree.get_ancestry(best);
        let tip_ancestry = self.fork_tree.get_ancestry(self.current_tip);

        // Find common ancestor
        let tip_set: HashSet<u64> = tip_ancestry.iter().copied().collect();
        let common = *best_ancestry.iter().find(|s| tip_set.contains(s))?;

        // If common ancestor == current_tip, best is a descendant — no switch needed
        if common == self.current_tip {
            return None;
        }

        // Check Tower lockout allows switching
        if self.tower.check_vote_lockout(best).is_err() {
            // Check switch threshold (38%) — need enough stake on alternative fork
            let alt_stake = self.fork_tree.get_node(best)?.subtree_stake;
            let total = self.fork_tree.total_active_stake();
            if total == 0
                || ((alt_stake as u128 * 100 / total as u128) as u64)
                    < crate::tower::SWITCH_THRESHOLD_PERCENTAGE
            {
                return None;
            }
        }

        // Build replay path: common_ancestor → ... → best
        let mut replay_slots: Vec<u64> = best_ancestry
            .into_iter()
            .take_while(|s| *s != common)
            .collect();
        replay_slots.reverse();

        Some(ForkSwitchPlan {
            common_ancestor: common,
            rollback_from: self.current_tip,
            replay_slots,
        })
    }
}
