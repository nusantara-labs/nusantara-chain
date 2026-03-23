use borsh::{BorshDeserialize, BorshSerialize};

use crate::hash::{Hash, hashv};

fn hash_leaf(data: &Hash) -> Hash {
    hashv(&[&[0x00], data.as_bytes()])
}

fn hash_internal(left: &Hash, right: &Hash) -> Hash {
    hashv(&[&[0x01], left.as_bytes(), right.as_bytes()])
}

fn next_power_of_two(n: usize) -> usize {
    n.next_power_of_two()
}

#[derive(Clone, Debug)]
pub struct MerkleTree {
    nodes: Vec<Hash>,
    leaf_count: usize,
}

impl MerkleTree {
    pub fn new(leaves: &[Hash]) -> Self {
        if leaves.is_empty() {
            return Self {
                nodes: vec![Hash::zero()],
                leaf_count: 0,
            };
        }

        let padded_count = next_power_of_two(leaves.len());
        let total_nodes = 2 * padded_count - 1;
        let mut nodes = vec![Hash::zero(); total_nodes];

        // Fill leaf layer
        for (i, leaf) in leaves.iter().enumerate() {
            nodes[padded_count - 1 + i] = hash_leaf(leaf);
        }
        // Pad remaining leaves with Hash::zero() hashed as leaf
        for i in leaves.len()..padded_count {
            nodes[padded_count - 1 + i] = hash_leaf(&Hash::zero());
        }

        // Build internal nodes bottom-up
        for i in (0..padded_count - 1).rev() {
            let left = &nodes[2 * i + 1];
            let right = &nodes[2 * i + 2];
            nodes[i] = hash_internal(left, right);
        }

        Self {
            nodes,
            leaf_count: leaves.len(),
        }
    }

    pub fn root(&self) -> Hash {
        self.nodes[0]
    }

    pub fn leaf_count(&self) -> usize {
        self.leaf_count
    }

    pub fn proof(&self, index: usize) -> Option<MerkleProof> {
        if index >= self.leaf_count {
            return None;
        }

        let padded_count = self.nodes.len().div_ceil(2);
        let mut pos = padded_count - 1 + index;
        let mut hashes = Vec::new();
        let mut path = Vec::new();

        while pos > 0 {
            let sibling = if pos % 2 == 1 { pos + 1 } else { pos - 1 };
            hashes.push(self.nodes[sibling]);
            path.push(pos.is_multiple_of(2)); // true if current node is right child
            pos = (pos - 1) / 2;
        }

        Some(MerkleProof { hashes, path })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct MerkleProof {
    pub hashes: Vec<Hash>,
    pub path: Vec<bool>,
}

impl MerkleProof {
    pub fn verify(&self, leaf: &Hash, root: &Hash) -> bool {
        let mut current = hash_leaf(leaf);

        for (sibling, is_right) in self.hashes.iter().zip(self.path.iter()) {
            current = if *is_right {
                hash_internal(sibling, &current)
            } else {
                hash_internal(&current, sibling)
            };
        }

        current == *root
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hash::hash;

    #[test]
    fn single_leaf() {
        let leaf = hash(b"leaf");
        let tree = MerkleTree::new(&[leaf]);
        let proof = tree.proof(0).unwrap();
        assert!(proof.verify(&leaf, &tree.root()));
    }

    #[test]
    fn two_leaves() {
        let leaves: Vec<Hash> = (0..2).map(|i| hash(&[i])).collect();
        let tree = MerkleTree::new(&leaves);
        for (i, leaf) in leaves.iter().enumerate() {
            let proof = tree.proof(i).unwrap();
            assert!(proof.verify(leaf, &tree.root()));
        }
    }

    #[test]
    fn four_leaves() {
        let leaves: Vec<Hash> = (0..4).map(|i| hash(&[i])).collect();
        let tree = MerkleTree::new(&leaves);
        for (i, leaf) in leaves.iter().enumerate() {
            let proof = tree.proof(i).unwrap();
            assert!(proof.verify(leaf, &tree.root()));
        }
    }

    #[test]
    fn non_power_of_two_leaves() {
        let leaves: Vec<Hash> = (0..5).map(|i| hash(&[i])).collect();
        let tree = MerkleTree::new(&leaves);
        for (i, leaf) in leaves.iter().enumerate() {
            let proof = tree.proof(i).unwrap();
            assert!(proof.verify(leaf, &tree.root()));
        }
    }

    #[test]
    fn tampered_leaf_fails() {
        let leaves: Vec<Hash> = (0..4).map(|i| hash(&[i])).collect();
        let tree = MerkleTree::new(&leaves);
        let proof = tree.proof(0).unwrap();
        let fake_leaf = hash(b"fake");
        assert!(!proof.verify(&fake_leaf, &tree.root()));
    }

    #[test]
    fn empty_tree() {
        let tree = MerkleTree::new(&[]);
        assert_eq!(tree.root(), Hash::zero());
        assert!(tree.proof(0).is_none());
    }

    #[test]
    fn proof_out_of_bounds() {
        let leaves: Vec<Hash> = (0..3).map(|i| hash(&[i])).collect();
        let tree = MerkleTree::new(&leaves);
        assert!(tree.proof(3).is_none());
    }

    #[test]
    fn deterministic() {
        let leaves: Vec<Hash> = (0..8).map(|i| hash(&[i])).collect();
        let tree1 = MerkleTree::new(&leaves);
        let tree2 = MerkleTree::new(&leaves);
        assert_eq!(tree1.root(), tree2.root());
    }

    #[test]
    fn borsh_roundtrip() {
        let leaves: Vec<Hash> = (0..4).map(|i| hash(&[i])).collect();
        let tree = MerkleTree::new(&leaves);
        let proof = tree.proof(0).unwrap();
        let encoded = borsh::to_vec(&proof).unwrap();
        let decoded: MerkleProof = borsh::from_slice(&encoded).unwrap();
        assert_eq!(proof, decoded);
        assert!(decoded.verify(&leaves[0], &tree.root()));
    }
}
