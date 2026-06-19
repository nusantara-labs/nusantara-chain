use std::collections::{HashMap, HashSet, VecDeque};

use nusantara_core::commitment::CommitmentLevel;
use nusantara_core::native_token::const_parse_u64;
use nusantara_crypto::Hash;
use tracing::instrument;

use crate::error::ConsensusError;

pub const MAX_UNCONFIRMED_DEPTH: u64 =
    const_parse_u64(env!("NUSA_FORK_CHOICE_MAX_UNCONFIRMED_DEPTH"));
pub const DUPLICATE_THRESHOLD_PERCENTAGE: u64 =
    const_parse_u64(env!("NUSA_FORK_CHOICE_DUPLICATE_THRESHOLD_PERCENTAGE"));

/// B28: hoisted constant for max fork tree nodes.
pub const MAX_FORK_TREE_NODES: usize = MAX_UNCONFIRMED_DEPTH as usize * 4;

#[derive(Clone, Debug)]
pub struct ForkNode {
    pub slot: u64,
    pub parent_slot: Option<u64>,
    pub block_hash: Hash,
    pub bank_hash: Hash,
    pub children: Vec<u64>,
    pub stake_voted: u64,
    pub subtree_stake: u64,
    pub is_connected: bool,
    pub commitment: CommitmentLevel,
    /// Voters whose stake has already been counted for this slot (B1 dedup).
    pub voters_seen: HashSet<Hash>,
}

pub struct ForkTree {
    nodes: HashMap<u64, ForkNode>,
    root_slot: u64,
    best_slot: u64,
    total_active_stake: u64,
    /// First slot each voter cast a vote on — prevents double-counting across
    /// slots in the same fork session (B1: first-vote-wins).
    validator_to_voted_slot: HashMap<Hash, u64>,
}

impl ForkTree {
    pub fn new(root_slot: u64, block_hash: Hash, bank_hash: Hash) -> Self {
        let mut nodes = HashMap::new();
        nodes.insert(
            root_slot,
            ForkNode {
                slot: root_slot,
                parent_slot: None,
                block_hash,
                bank_hash,
                children: Vec::new(),
                stake_voted: 0,
                subtree_stake: 0,
                is_connected: true,
                commitment: CommitmentLevel::Finalized,
                voters_seen: HashSet::new(),
            },
        );
        Self {
            nodes,
            root_slot,
            best_slot: root_slot,
            total_active_stake: 0,
            validator_to_voted_slot: HashMap::new(),
        }
    }

    pub fn set_total_active_stake(&mut self, stake: u64) {
        self.total_active_stake = stake;
    }

    pub fn root_slot(&self) -> u64 {
        self.root_slot
    }

    pub fn best_slot(&self) -> u64 {
        self.best_slot
    }

    pub fn contains(&self, slot: u64) -> bool {
        self.nodes.contains_key(&slot)
    }

    pub fn get_node(&self, slot: u64) -> Option<&ForkNode> {
        self.nodes.get(&slot)
    }

    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// Add a new slot to the fork tree.
    #[instrument(skip(self), level = "debug")]
    pub fn add_slot(
        &mut self,
        slot: u64,
        parent_slot: u64,
        block_hash: Hash,
        bank_hash: Hash,
    ) -> Result<(), ConsensusError> {
        if self.nodes.contains_key(&slot) {
            return Err(ConsensusError::SlotAlreadyProcessed(slot));
        }

        // Cap total nodes to prevent unbounded memory growth (B28: use hoisted constant).
        if self.nodes.len() >= MAX_FORK_TREE_NODES {
            return Err(ConsensusError::MaxDepthExceeded {
                depth: self.nodes.len() as u64,
                max: MAX_FORK_TREE_NODES as u64,
            });
        }

        let is_connected = if let Some(parent) = self.nodes.get_mut(&parent_slot) {
            parent.children.push(slot);
            parent.is_connected
        } else {
            // Orphan slot: parent not yet in tree
            false
        };

        self.nodes.insert(
            slot,
            ForkNode {
                slot,
                parent_slot: Some(parent_slot),
                block_hash,
                bank_hash,
                children: Vec::new(),
                stake_voted: 0,
                subtree_stake: 0,
                is_connected,
                commitment: CommitmentLevel::Processed,
                voters_seen: HashSet::new(),
            },
        );

        metrics::gauge!("nusantara_fork_tree_node_count").set(self.nodes.len() as f64);
        Ok(())
    }

    /// Add a vote for a slot from `voter`, propagating subtree_stake up to root.
    ///
    /// Returns `false` if:
    /// - The slot is not in the fork tree (vote dropped).
    /// - The voter already voted for this exact slot (idempotent same-slot revote).
    /// - The voter previously voted for a *different* slot (cross-slot revote rejected).
    ///   Tower BFT lockout enforces single-slot-per-validator externally; ForkTree
    ///   must not silently switch the recorded vote and double-count ancestor stake.
    #[instrument(skip(self), level = "debug")]
    pub fn add_vote(&mut self, slot: u64, voter: Hash, stake: u64) -> bool {
        // B1 cross-slot guard: check validator_to_voted_slot before touching any node.
        if let Some(&prev_slot) = self.validator_to_voted_slot.get(&voter) {
            if prev_slot == slot {
                // Idempotent same-slot revote — no change.
                return false;
            }
            // Cross-slot revote rejected. The Tower lockout logic is responsible
            // for ensuring validators don't vote on conflicting slots; accepting
            // this here would double-count ancestor stake.
            return false;
        }

        // First vote from this validator for this slot.
        if let Some(node) = self.nodes.get_mut(&slot) {
            // voters_seen provides an additional per-node sanity check.
            if !node.voters_seen.insert(voter) {
                // Should not reach here given the validator_to_voted_slot guard above,
                // but guard defensively.
                return false;
            }
            node.stake_voted = node.stake_voted.saturating_add(stake);
        } else {
            return false;
        }

        // Record this voter's slot before propagating so any re-entrant call is rejected.
        self.validator_to_voted_slot.insert(voter, slot);

        // Propagate subtree_stake up the ancestry chain.
        let mut current = slot;
        while let Some(node) = self.nodes.get_mut(&current) {
            node.subtree_stake = node.subtree_stake.saturating_add(stake);
            match node.parent_slot {
                Some(parent) if parent != current => current = parent,
                _ => break,
            }
        }
        true
    }

    /// Compute the best (heaviest) fork by walking the tree.
    #[instrument(skip(self), level = "debug")]
    pub fn compute_best_fork(&mut self) -> u64 {
        let best = self.find_heaviest_from(self.root_slot);
        self.best_slot = best;
        metrics::gauge!("nusantara_fork_tree_best_slot").set(best as f64);
        best
    }

    fn find_heaviest_from(&self, start: u64) -> u64 {
        let mut current = start;
        loop {
            let Some(node) = self.nodes.get(&current) else {
                return current;
            };
            if node.children.is_empty() {
                return current;
            }
            match node
                .children
                .iter()
                .filter_map(|&cs| self.nodes.get(&cs).map(|c| (cs, c)))
                .max_by_key(|(_, c)| (c.subtree_stake, c.block_hash))
            {
                Some((best, _)) => current = best,
                None => return current,
            }
        }
    }

    /// Set a new root, pruning all slots that are not ancestors of the new root.
    /// Returns the list of pruned slot numbers, or `Err(InvalidRoot)` if
    /// `new_root` is not present in the fork tree (B2).
    #[instrument(skip(self), level = "info")]
    pub fn set_root(&mut self, new_root: u64) -> Result<Vec<u64>, ConsensusError> {
        if !self.nodes.contains_key(&new_root) {
            return Err(ConsensusError::InvalidRoot(new_root));
        }
        if new_root <= self.root_slot {
            return Ok(Vec::new());
        }

        // Find ancestry from new_root to old root
        let ancestry = self.get_ancestry(new_root);
        let ancestry_set: std::collections::HashSet<u64> = ancestry.iter().copied().collect();

        // Collect all slots reachable from new_root
        let mut reachable = std::collections::HashSet::new();
        self.collect_subtree(new_root, &mut reachable);

        // Prune everything not in ancestry or reachable from new_root (single-pass, no alloc)
        let mut pruned = Vec::new();
        self.nodes.retain(|&slot, _| {
            let keep = reachable.contains(&slot) || ancestry_set.contains(&slot);
            if !keep {
                pruned.push(slot);
            }
            keep
        });

        // Remove ancestry nodes between old root and new root (exclusive)
        for &slot in &ancestry {
            if slot != new_root && slot != self.root_slot {
                // Remove from tree but mark as pruned
                if let Some(removed) = self.nodes.remove(&slot) {
                    let _ = removed;
                    pruned.push(slot);
                }
            }
        }
        // Remove old root if different from new root
        if self.root_slot != new_root && self.nodes.remove(&self.root_slot).is_some() {
            pruned.push(self.root_slot);
        }

        // Update new root
        if let Some(root_node) = self.nodes.get_mut(&new_root) {
            root_node.parent_slot = None;
            root_node.commitment = CommitmentLevel::Finalized;
        }

        self.root_slot = new_root;
        // Also clear the per-validator slot tracking for the pruned history —
        // after root advance the old assignments are stale.
        self.validator_to_voted_slot.retain(|_, voted_slot| *voted_slot >= new_root);
        metrics::gauge!("nusantara_fork_tree_root_slot").set(new_root as f64);
        metrics::gauge!("nusantara_fork_tree_node_count").set(self.nodes.len() as f64);

        Ok(pruned)
    }

    fn collect_subtree(&self, root: u64, reachable: &mut std::collections::HashSet<u64>) {
        let mut stack = vec![root];
        while let Some(slot) = stack.pop() {
            reachable.insert(slot);
            if let Some(node) = self.nodes.get(&slot) {
                stack.extend(&node.children);
            }
        }
    }

    /// Get ancestry chain from slot up to root.
    #[instrument(skip(self), level = "debug")]
    pub fn get_ancestry(&self, slot: u64) -> Vec<u64> {
        let mut chain = Vec::new();
        let mut current = slot;

        loop {
            chain.push(current);
            match self.nodes.get(&current) {
                Some(node) => match node.parent_slot {
                    Some(parent) if parent != current => current = parent,
                    _ => break,
                },
                None => break,
            }
        }

        chain
    }

    pub fn total_active_stake(&self) -> u64 {
        self.total_active_stake
    }

    /// Return the highest slot number in the fork tree.
    #[instrument(skip(self), level = "debug")]
    pub fn latest_slot(&self) -> Option<u64> {
        self.nodes.keys().max().copied()
    }

    /// Try to reconnect orphan slots after their parent is added.
    ///
    /// B17: single-pass BFS — O(N) instead of the previous O(N²) fixed-point loop.
    /// Builds a parent→children map for orphan nodes once, then BFS from all
    /// already-connected nodes to propagate `is_connected` to their orphan children.
    #[instrument(skip(self), level = "debug")]
    pub fn try_reconnect_orphans(&mut self) {
        // Build orphan_children: parent_slot -> Vec<child_slot> for disconnected nodes.
        let mut orphan_children: HashMap<u64, Vec<u64>> = HashMap::new();
        for (&slot, node) in &self.nodes {
            if !node.is_connected && let Some(parent) = node.parent_slot {
                orphan_children.entry(parent).or_default().push(slot);
            }
        }

        if orphan_children.is_empty() {
            return;
        }

        // BFS: start from all currently connected nodes.
        let mut queue: VecDeque<u64> = self
            .nodes
            .iter()
            .filter(|(_, n)| n.is_connected)
            .map(|(&s, _)| s)
            .collect();

        while let Some(parent_slot) = queue.pop_front() {
            let Some(children) = orphan_children.remove(&parent_slot) else {
                continue;
            };
            for child_slot in children {
                if let Some(child_node) = self.nodes.get_mut(&child_slot) {
                    child_node.is_connected = true;
                }
                // Register child in parent's children list if absent.
                if let Some(parent_node) = self.nodes.get_mut(&parent_slot)
                    && !parent_node.children.contains(&child_slot)
                {
                    parent_node.children.push(child_slot);
                }
                queue.push_back(child_slot);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nusantara_crypto::hash;

    fn h(s: &str) -> Hash {
        hash(s.as_bytes())
    }

    fn voter(s: &str) -> Hash {
        hash(s.as_bytes())
    }

    #[test]
    fn config_values() {
        assert_eq!(MAX_UNCONFIRMED_DEPTH, 64);
        assert_eq!(DUPLICATE_THRESHOLD_PERCENTAGE, 52);
        assert_eq!(MAX_FORK_TREE_NODES, 256);
    }

    #[test]
    fn new_tree() {
        let tree = ForkTree::new(0, h("genesis_block"), h("genesis_bank"));
        assert_eq!(tree.root_slot(), 0);
        assert_eq!(tree.best_slot(), 0);
        assert_eq!(tree.node_count(), 1);
    }

    #[test]
    fn add_linear_chain() {
        let mut tree = ForkTree::new(0, h("b0"), h("bk0"));
        for slot in 1..=10 {
            tree.add_slot(
                slot,
                slot - 1,
                h(&format!("b{slot}")),
                h(&format!("bk{slot}")),
            )
            .unwrap();
        }
        assert_eq!(tree.node_count(), 11);
    }

    #[test]
    fn add_duplicate_slot_fails() {
        let mut tree = ForkTree::new(0, h("b0"), h("bk0"));
        tree.add_slot(1, 0, h("b1"), h("bk1")).unwrap();
        let result = tree.add_slot(1, 0, h("b1"), h("bk1"));
        assert!(result.is_err());
    }

    #[test]
    fn heaviest_fork_selection() {
        let mut tree = ForkTree::new(0, h("b0"), h("bk0"));
        // Fork A: 0 -> 1 -> 2
        tree.add_slot(1, 0, h("b1"), h("bk1")).unwrap();
        tree.add_slot(2, 1, h("b2"), h("bk2")).unwrap();
        // Fork B: 0 -> 3 -> 4
        tree.add_slot(3, 0, h("b3"), h("bk3")).unwrap();
        tree.add_slot(4, 3, h("b4"), h("bk4")).unwrap();

        // Add more stake to fork A
        tree.add_vote(2, voter("v1"), 100);
        // Less stake to fork B
        tree.add_vote(4, voter("v2"), 50);

        let best = tree.compute_best_fork();
        assert_eq!(best, 2); // Fork A is heavier
    }

    #[test]
    fn vote_propagation() {
        let mut tree = ForkTree::new(0, h("b0"), h("bk0"));
        tree.add_slot(1, 0, h("b1"), h("bk1")).unwrap();
        tree.add_slot(2, 1, h("b2"), h("bk2")).unwrap();

        tree.add_vote(2, voter("v1"), 100);

        // Stake should propagate up
        assert_eq!(tree.get_node(2).unwrap().subtree_stake, 100);
        assert_eq!(tree.get_node(1).unwrap().subtree_stake, 100);
        assert_eq!(tree.get_node(0).unwrap().subtree_stake, 100);
    }

    #[test]
    fn vote_dedup_same_voter() {
        let mut tree = ForkTree::new(0, h("b0"), h("bk0"));
        tree.add_slot(1, 0, h("b1"), h("bk1")).unwrap();

        let v = voter("alice");
        assert!(tree.add_vote(1, v, 500));
        // Second vote from same voter must be rejected
        assert!(!tree.add_vote(1, v, 500));
        // Stake must not be doubled
        assert_eq!(tree.get_node(1).unwrap().stake_voted, 500);
        assert_eq!(tree.get_node(1).unwrap().subtree_stake, 500);
    }

    #[test]
    fn set_root_prunes() {
        let mut tree = ForkTree::new(0, h("b0"), h("bk0"));
        // Fork A: 0 -> 1 -> 2 -> 3
        tree.add_slot(1, 0, h("b1"), h("bk1")).unwrap();
        tree.add_slot(2, 1, h("b2"), h("bk2")).unwrap();
        tree.add_slot(3, 2, h("b3"), h("bk3")).unwrap();
        // Fork B: 0 -> 4
        tree.add_slot(4, 0, h("b4"), h("bk4")).unwrap();

        let pruned = tree.set_root(2).unwrap();
        assert!(pruned.contains(&4)); // Fork B pruned
        assert!(pruned.contains(&0)); // Old root pruned
        assert!(pruned.contains(&1)); // Intermediate pruned
        assert!(tree.contains(2)); // New root exists
        assert!(tree.contains(3)); // Child of new root exists
        assert_eq!(tree.root_slot(), 2);
    }

    #[test]
    fn set_root_nonexistent_returns_error() {
        let mut tree = ForkTree::new(0, h("b0"), h("bk0"));
        tree.add_slot(1, 0, h("b1"), h("bk1")).unwrap();
        // Slot 99 does not exist — must return Err(InvalidRoot)
        let result = tree.set_root(99);
        assert!(matches!(result, Err(ConsensusError::InvalidRoot(99))));
    }

    #[test]
    fn get_ancestry() {
        let mut tree = ForkTree::new(0, h("b0"), h("bk0"));
        tree.add_slot(1, 0, h("b1"), h("bk1")).unwrap();
        tree.add_slot(2, 1, h("b2"), h("bk2")).unwrap();
        tree.add_slot(3, 2, h("b3"), h("bk3")).unwrap();

        let ancestry = tree.get_ancestry(3);
        assert_eq!(ancestry, vec![3, 2, 1, 0]);
    }

    #[test]
    fn reorg_test() {
        let mut tree = ForkTree::new(0, h("b0"), h("bk0"));
        tree.add_slot(1, 0, h("b1"), h("bk1")).unwrap();
        tree.add_slot(2, 0, h("b2"), h("bk2")).unwrap();

        // Fork A initially heaviest
        tree.add_vote(1, voter("v1"), 100);
        assert_eq!(tree.compute_best_fork(), 1);

        // Fork B overtakes
        tree.add_vote(2, voter("v2"), 200);
        assert_eq!(tree.compute_best_fork(), 2);
    }

    /// F1 regression: voter V votes slot 5 then attempts a cross-slot revote on
    /// slot 3 (an ancestor). The second call must return false and the ancestor
    /// subtree_stake must remain unchanged from after the first vote.
    #[test]
    fn cross_slot_revote_rejected() {
        let mut tree = ForkTree::new(0, h("b0"), h("bk0"));
        // Build chain: 0 -> 1 -> 2 -> 3 -> 4 -> 5
        for slot in 1..=5 {
            tree.add_slot(slot, slot - 1, h(&format!("b{slot}")), h(&format!("bk{slot}")))
                .unwrap();
        }

        let v = voter("validator_v");

        // First vote: V votes for slot 5 with 100 stake.
        assert!(tree.add_vote(5, v, 100));

        // Stake must have propagated to all ancestors.
        for anc in 0..=5 {
            assert_eq!(
                tree.get_node(anc).unwrap().subtree_stake,
                100,
                "slot {anc} should have subtree_stake=100 after first vote"
            );
        }

        // Cross-slot revote: V tries to vote for slot 3 — must be rejected.
        assert!(!tree.add_vote(3, v, 100));

        // Ancestor subtree_stake must be unchanged (no double-count).
        for anc in 0..=5 {
            assert_eq!(
                tree.get_node(anc).unwrap().subtree_stake,
                100,
                "slot {anc} subtree_stake must not change after rejected cross-slot revote"
            );
        }
    }

    /// Idempotent same-slot revote must also return false and not change stake.
    #[test]
    fn same_slot_revote_idempotent() {
        let mut tree = ForkTree::new(0, h("b0"), h("bk0"));
        tree.add_slot(1, 0, h("b1"), h("bk1")).unwrap();

        let v = voter("v_idem");
        assert!(tree.add_vote(1, v, 200));
        assert!(!tree.add_vote(1, v, 200)); // same slot — idempotent, no change
        assert_eq!(tree.get_node(1).unwrap().stake_voted, 200);
        assert_eq!(tree.get_node(0).unwrap().subtree_stake, 200);
    }
}
