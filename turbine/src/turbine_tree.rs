use std::collections::HashMap;

use nusantara_core::native_token::const_parse_u64;
use nusantara_crypto::{Hash, hashv};

pub const TURBINE_FANOUT: u64 = const_parse_u64(env!("NUSA_TURBINE_FANOUT"));

pub struct TurbineTree {
    leader: Hash,
    nodes: Vec<Hash>,
    fanout: usize,
}

impl TurbineTree {
    /// Build a turbine tree for the given slot.
    /// `cluster_nodes` is all nodes (including leader).
    /// `stakes` maps identity -> stake amount (O(1) lookup).
    pub fn new(
        leader: Hash,
        cluster_nodes: &[Hash],
        stakes: &HashMap<Hash, u64>,
        slot: u64,
        fanout: usize,
    ) -> Self {
        // Deterministic seed per slot
        let seed = hashv(&[&slot.to_le_bytes(), leader.as_bytes()]);

        // Filter out leader from relay nodes
        let non_leader: Vec<(Hash, u64)> = cluster_nodes
            .iter()
            .filter(|n| **n != leader)
            .map(|n| {
                let stake = stakes.get(n).copied().unwrap_or(1);
                (*n, stake)
            })
            .collect();

        // Stake-weighted deterministic shuffle
        let shuffled = weighted_shuffle_turbine(&non_leader, &seed);

        Self {
            leader,
            nodes: shuffled,
            fanout,
        }
    }

    /// Get the peers that `my_identity` should retransmit to.
    pub fn retransmit_peers(&self, my_identity: &Hash) -> Vec<Hash> {
        if *my_identity == self.leader {
            // Leader sends to layer 0 (first `fanout` nodes)
            return self.nodes.iter().take(self.fanout).copied().collect();
        }

        // Find our position in the ordered list
        let my_pos = match self.nodes.iter().position(|n| n == my_identity) {
            Some(pos) => pos,
            None => return Vec::new(),
        };

        // Compute our layer: layer 0 = first `fanout` nodes, etc.
        let layer = my_pos / self.fanout;
        let next_layer_start = (layer + 1) * self.fanout;

        // Our position within our layer
        let pos_in_layer = my_pos % self.fanout;

        // Children in next layer
        let child_start = next_layer_start + pos_in_layer * self.fanout;
        let child_end = (child_start + self.fanout).min(self.nodes.len());

        if child_start >= self.nodes.len() {
            return Vec::new();
        }

        self.nodes[child_start..child_end].to_vec()
    }

    /// Which layer is this node in? Layer 0 is directly connected to leader.
    pub fn layer_of(&self, identity: &Hash) -> Option<usize> {
        if *identity == self.leader {
            return None; // Leader is not in any layer
        }
        self.nodes
            .iter()
            .position(|n| n == identity)
            .map(|pos| pos / self.fanout)
    }

    pub fn leader(&self) -> Hash {
        self.leader
    }

    pub fn total_nodes(&self) -> usize {
        self.nodes.len() + 1 // +1 for leader
    }
}

/// Fixed-point scale factor for u128 integer arithmetic (10^18).
const SCALE: u128 = 1_000_000_000_000_000_000;

/// Internal stake-weighted shuffle for turbine tree ordering.
/// Uses u128 fixed-point arithmetic for cross-platform determinism.
fn weighted_shuffle_turbine(nodes: &[(Hash, u64)], seed: &Hash) -> Vec<Hash> {
    if nodes.is_empty() {
        return Vec::new();
    }

    let total_stake: u64 = nodes.iter().map(|(_, s)| *s).sum();
    if total_stake == 0 {
        return nodes.iter().map(|(h, _)| *h).collect();
    }

    let mut weighted: Vec<(usize, u128)> = nodes
        .iter()
        .enumerate()
        .map(|(i, (identity, stake))| {
            let h = hashv(&[seed.as_bytes(), identity.as_bytes()]);
            let bytes = h.as_bytes();
            let rand_val = u64::from_le_bytes([
                bytes[0], bytes[1], bytes[2], bytes[3],
                bytes[4], bytes[5], bytes[6], bytes[7],
            ]);
            let stake_component = (*stake as u128) * SCALE / (total_stake as u128);
            let rand_component =
                (rand_val as u128) * (SCALE / 100) / (u64::MAX as u128);
            let weight = stake_component + rand_component;
            (i, weight)
        })
        .collect();

    weighted.sort_by(|a, b| b.1.cmp(&a.1));
    weighted.into_iter().map(|(i, _)| nodes[i].0).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use nusantara_crypto::hash;

    #[test]
    fn config_values() {
        assert_eq!(TURBINE_FANOUT, 32);
    }

    #[test]
    fn leader_sends_to_layer_0() {
        let leader = hash(b"leader");
        let nodes: Vec<Hash> = (0..100).map(|i| hash(&(i as u64).to_le_bytes())).collect();
        let stakes: HashMap<Hash, u64> = nodes.iter().map(|n| (*n, 100)).collect();

        let tree = TurbineTree::new(leader, &nodes, &stakes, 1, 32);
        let peers = tree.retransmit_peers(&leader);
        assert_eq!(peers.len(), 32);
    }

    #[test]
    fn layer_0_node_sends_to_layer_1() {
        let leader = hash(b"leader");
        let nodes: Vec<Hash> = (0..200).map(|i| hash(&(i as u64).to_le_bytes())).collect();
        let stakes: HashMap<Hash, u64> = nodes.iter().map(|n| (*n, 100)).collect();

        let tree = TurbineTree::new(leader, &nodes, &stakes, 1, 4);

        // Find a layer-0 node
        let layer_0_peers = tree.retransmit_peers(&leader);
        let layer_0_node = &layer_0_peers[0];

        let peers = tree.retransmit_peers(layer_0_node);
        // Should have children in layer 1
        assert!(!peers.is_empty());
    }

    #[test]
    fn deterministic_per_slot() {
        let leader = hash(b"leader");
        let nodes: Vec<Hash> = (0..50).map(|i| hash(&(i as u64).to_le_bytes())).collect();
        let stakes: HashMap<Hash, u64> = nodes.iter().map(|n| (*n, 100)).collect();

        let tree1 = TurbineTree::new(leader, &nodes, &stakes, 5, 32);
        let tree2 = TurbineTree::new(leader, &nodes, &stakes, 5, 32);

        let peers1 = tree1.retransmit_peers(&leader);
        let peers2 = tree2.retransmit_peers(&leader);
        assert_eq!(peers1, peers2);
    }

    #[test]
    fn different_slots_different_topology() {
        let leader = hash(b"leader");
        let nodes: Vec<Hash> = (0..50).map(|i| hash(&(i as u64).to_le_bytes())).collect();
        let stakes: HashMap<Hash, u64> = nodes.iter().map(|n| (*n, 100)).collect();

        let tree1 = TurbineTree::new(leader, &nodes, &stakes, 1, 32);
        let tree2 = TurbineTree::new(leader, &nodes, &stakes, 2, 32);

        let peers1 = tree1.retransmit_peers(&leader);
        let peers2 = tree2.retransmit_peers(&leader);
        assert_ne!(peers1, peers2);
    }

    #[test]
    fn unknown_node_empty_peers() {
        let leader = hash(b"leader");
        let nodes: Vec<Hash> = (0..10).map(|i| hash(&(i as u64).to_le_bytes())).collect();
        let stakes: HashMap<Hash, u64> = nodes.iter().map(|n| (*n, 100)).collect();

        let tree = TurbineTree::new(leader, &nodes, &stakes, 1, 4);
        let unknown = hash(b"unknown");
        assert!(tree.retransmit_peers(&unknown).is_empty());
    }

    #[test]
    fn layer_of_calculation() {
        let leader = hash(b"leader");
        let nodes: Vec<Hash> = (0..100).map(|i| hash(&(i as u64).to_le_bytes())).collect();
        let stakes: HashMap<Hash, u64> = nodes.iter().map(|n| (*n, 100)).collect();

        let tree = TurbineTree::new(leader, &nodes, &stakes, 1, 10);
        assert_eq!(tree.layer_of(&leader), None); // Leader has no layer
    }
}
