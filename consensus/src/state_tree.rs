use std::collections::{BTreeMap, HashSet};

use nusantara_core::Account;
use nusantara_crypto::{Hash, Hasher, hashv};
use nusantara_storage::Storage;
use tracing::instrument;

use crate::error::ConsensusError;

/// Merkle proof for a single account in the state tree.
///
/// Contains the sibling hashes from the leaf level up to the root,
/// along with a path indicating whether the current node was the
/// right child at each level.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StateMerkleProof {
    /// Sibling hashes from leaf level up to the root.
    pub siblings: Vec<Hash>,
    /// For each level, `true` if the current node was the right child.
    pub path: Vec<bool>,
    /// Leaf index in the sorted leaf array.
    pub leaf_index: usize,
    /// Total number of leaves when the proof was generated.
    pub total_leaves: usize,
}

/// Incremental state Merkle tree over all accounts.
///
/// Maintains a sorted mapping of account addresses to their leaf hashes.
/// The tree is a standard binary Merkle tree using power-of-two padding
/// (matching the existing `MerkleTree` in the crypto crate) but operates
/// over account state rather than transaction hashes.
///
/// # Incremental dirty-path tracking
///
/// When only existing accounts are updated (the hot path during steady-state
/// block production), the tree avoids a full O(N log N) rebuild. Instead it:
///
/// 1. Updates the affected leaf node in the persistent `nodes` array.
/// 2. Marks every ancestor of that leaf as *dirty*.
/// 3. On the next `root()` call, recomputes only the dirty internal nodes
///    in bottom-up order -- O(D * log N) where D is the number of changed leaves.
///
/// Structural changes (new account creation, account removal) invalidate
/// the entire node array and trigger a full rebuild on the next `root()` call,
/// because the sorted BTreeMap ordering assigns positions to leaves and any
/// insertion or removal shifts subsequent leaf indices.
///
/// Leaf hash = hashv(&[b"state_leaf", address_bytes, borsh(account)])
/// Internal nodes use a 0x01 domain separator to prevent second-preimage attacks.
pub struct StateTree {
    /// Sorted by address for deterministic ordering.
    /// address -> leaf_hash
    leaves: BTreeMap<Hash, Hash>,
    /// Flat binary tree: nodes[0] = root, left child = 2*i+1, right child = 2*i+2.
    /// Leaf layer starts at index `capacity - 1`.
    /// Empty when the tree has never been built or after a structural change.
    nodes: Vec<Hash>,
    /// Current capacity (always a power of two, number of leaf slots in node array).
    /// Zero when the node array is empty.
    capacity: usize,
    /// True if any address was added or removed since the last full rebuild.
    /// When set, the next `root()` or `ensure_rebuilt()` call will do a full rebuild.
    structural_change: bool,
    /// Internal node indices whose hashes are stale because a descendant leaf
    /// changed. Only valid when `!structural_change`. Sorted descending during
    /// flush so that children are recomputed before their parents.
    dirty: HashSet<usize>,
}

/// Hash a leaf node with a domain separator to avoid second-preimage attacks.
fn hash_leaf(data: &Hash) -> Hash {
    hashv(&[&[0x00], data.as_bytes()])
}

/// Hash two child nodes into a parent with a domain separator.
fn hash_internal(left: &Hash, right: &Hash) -> Hash {
    hashv(&[&[0x01], left.as_bytes(), right.as_bytes()])
}

/// Compute the leaf hash for an account at the given address.
///
/// Uses streaming hashing to avoid the intermediate `borsh::to_vec` allocation.
/// The byte sequence fed into the hasher is identical to
/// `hashv(&[b"state_leaf", address.as_bytes(), &borsh::to_vec(account)])`:
///
/// - `b"state_leaf"` (10 bytes)
/// - address (64 bytes, raw Hash)
/// - borsh(account): lamports(8 LE) + data_len(4 LE) + data(N) + owner(64) + executable(1) + rent_epoch(8 LE)
///
/// The `Hash` type's `BorshSerialize` writes exactly 64 raw bytes (no length prefix),
/// so `owner.as_bytes()` matches the borsh serialization of the `owner` field.
fn account_leaf_hash(address: &Hash, account: &Account) -> Hash {
    let mut hasher = Hasher::new();
    hasher.update(b"state_leaf");
    hasher.update(address.as_bytes());
    // Borsh serialization of Account fields in declaration order:
    hasher.update(&account.lamports.to_le_bytes());
    hasher.update(&(account.data.len() as u32).to_le_bytes());
    hasher.update(&account.data);
    hasher.update(account.owner.as_bytes());
    hasher.update(&[account.executable as u8]);
    hasher.update(&account.rent_epoch.to_le_bytes());
    hasher.finalize()
}

impl Default for StateTree {
    fn default() -> Self {
        Self::new()
    }
}

impl StateTree {
    /// Create an empty state tree.
    pub fn new() -> Self {
        Self {
            leaves: BTreeMap::new(),
            nodes: Vec::new(),
            capacity: 0,
            structural_change: false,
            dirty: HashSet::new(),
        }
    }

    /// Number of accounts tracked in the tree.
    pub fn len(&self) -> usize {
        self.leaves.len()
    }

    /// Returns true if the tree has no accounts.
    pub fn is_empty(&self) -> bool {
        self.leaves.is_empty()
    }

    /// Update leaf hashes for changed accounts.
    ///
    /// For existing addresses: updates the leaf hash in the BTreeMap and, if the
    /// persistent node array is valid, updates the corresponding leaf node in-place
    /// and marks its ancestors dirty for incremental recomputation.
    ///
    /// For new addresses: inserts into the BTreeMap and sets `structural_change`
    /// because the sorted leaf ordering shifts.
    #[instrument(skip_all, fields(delta_count = deltas.len()), level = "debug")]
    pub fn update(&mut self, deltas: &[(Hash, Account)]) {
        for (address, account) in deltas {
            let leaf_hash = account_leaf_hash(address, account);

            if self.leaves.contains_key(address) {
                // Existing account -- update in-place if node array is valid.
                self.leaves.insert(*address, leaf_hash);

                if !self.structural_change && !self.nodes.is_empty() {
                    // Find the sorted position of this address among all leaves.
                    // BTreeMap iteration order is sorted, so position() gives the
                    // correct leaf index.
                    let leaf_index = self
                        .leaves
                        .keys()
                        .position(|k| k == address)
                        .expect("key was just confirmed present");
                    let node_idx = self.capacity - 1 + leaf_index;
                    self.nodes[node_idx] = hash_leaf(&leaf_hash);
                    self.mark_ancestors_dirty(node_idx);
                }
            } else {
                // New account -- structural change, full rebuild needed.
                self.leaves.insert(*address, leaf_hash);
                self.structural_change = true;
            }
        }

        metrics::counter!("nusantara_state_tree_updates_total").increment(deltas.len() as u64);
    }

    /// Remove an account from the tree.
    ///
    /// Sets `structural_change` because removing a leaf shifts the sorted
    /// positions of subsequent leaves.
    pub fn remove(&mut self, address: &Hash) {
        if self.leaves.remove(address).is_some() {
            self.structural_change = true;
        }
    }

    /// Compute the Merkle root of all current leaves.
    ///
    /// Returns `Hash::zero()` for an empty tree.
    ///
    /// If only existing accounts were updated since the last call, this flushes
    /// dirty internal nodes in O(D * log N) where D is the number of changed
    /// leaves. If structural changes occurred, this performs a full O(N log N)
    /// rebuild.
    pub fn root(&mut self) -> Hash {
        if self.leaves.is_empty() {
            return Hash::zero();
        }

        self.ensure_rebuilt();
        self.flush_dirty();

        self.nodes[0]
    }

    /// Generate a Merkle proof for a specific account address.
    ///
    /// Returns `None` if the address is not in the tree or the tree is empty.
    /// Ensures the internal node array is up-to-date before generating the proof.
    pub fn proof(&mut self, address: &Hash) -> Option<StateMerkleProof> {
        if self.leaves.is_empty() {
            return None;
        }

        // Find the leaf index in sorted order before rebuilding (position is
        // stable across rebuild since BTreeMap order doesn't change).
        let leaf_index = self.leaves.keys().position(|k| k == address)?;
        let total_leaves = self.leaves.len();

        // Make sure the node array is current.
        self.ensure_rebuilt();
        self.flush_dirty();

        // Walk from leaf to root collecting siblings.
        let mut pos = self.capacity - 1 + leaf_index;
        let mut siblings = Vec::new();
        let mut path = Vec::new();

        while pos > 0 {
            let sibling = if pos % 2 == 1 { pos + 1 } else { pos - 1 };
            siblings.push(self.nodes[sibling]);
            path.push(pos.is_multiple_of(2)); // true if current node is right child
            pos = (pos - 1) / 2;
        }

        Some(StateMerkleProof {
            siblings,
            path,
            leaf_index,
            total_leaves,
        })
    }

    /// Verify a proof against a known root.
    ///
    /// Recomputes the leaf hash from the address and account, then walks
    /// up the proof path to see if the final hash matches the root.
    pub fn verify_proof(
        root: &Hash,
        address: &Hash,
        account: &Account,
        proof: &StateMerkleProof,
    ) -> bool {
        let leaf = account_leaf_hash(address, account);
        let mut current = hash_leaf(&leaf);

        for (sibling, is_right) in proof.siblings.iter().zip(proof.path.iter()) {
            current = if *is_right {
                hash_internal(sibling, &current)
            } else {
                hash_internal(&current, sibling)
            };
        }

        current == *root
    }

    /// Initialize the state tree from all accounts currently in storage.
    ///
    /// Loads every account via the storage public API, builds the leaf map,
    /// and performs a full build of the internal node array so that subsequent
    /// updates to existing accounts can use the incremental path.
    #[instrument(skip_all, level = "info")]
    pub fn init_from_storage(storage: &Storage) -> Result<Self, ConsensusError> {
        let all_accounts = storage.get_all_accounts()?;

        let mut leaves = BTreeMap::new();
        for (address, account) in &all_accounts {
            let leaf = account_leaf_hash(address, account);
            leaves.insert(*address, leaf);
        }

        let account_count = leaves.len();
        tracing::info!(account_count, "state tree initialized from storage");
        metrics::gauge!("nusantara_state_tree_leaf_count").set(account_count as f64);

        let mut tree = Self {
            leaves,
            nodes: Vec::new(),
            capacity: 0,
            structural_change: true,
            dirty: HashSet::new(),
        };

        // Pre-build the node array so the first slot after boot can use
        // incremental updates without a full rebuild.
        if !tree.leaves.is_empty() {
            tree.full_rebuild();
        }

        Ok(tree)
    }

    // ------------------------------------------------------------------
    // Private helpers
    // ------------------------------------------------------------------

    /// Mark all ancestors of `node_idx` as dirty (needing recomputation).
    /// Walks from the node up to (but not including) the root's children,
    /// inserting each parent index into the dirty set.
    fn mark_ancestors_dirty(&mut self, node_idx: usize) {
        let mut idx = node_idx;
        while idx > 0 {
            idx = (idx - 1) / 2;
            self.dirty.insert(idx);
        }
    }

    /// If a structural change occurred, perform a full rebuild of the flat
    /// node array from the current BTreeMap contents. Clears the dirty set
    /// and the structural_change flag.
    fn ensure_rebuilt(&mut self) {
        if self.structural_change || self.nodes.is_empty() {
            if self.leaves.is_empty() {
                self.nodes.clear();
                self.capacity = 0;
            } else {
                self.full_rebuild();
            }
            self.structural_change = false;
            self.dirty.clear();
        }
    }

    /// Flush all dirty internal nodes by recomputing them bottom-up.
    ///
    /// Collects dirty indices, sorts them in descending order so that deeper
    /// (higher-index) nodes are processed first, then recomputes each parent
    /// from its two children. This is O(|dirty| * 1) hash operations.
    fn flush_dirty(&mut self) {
        if self.dirty.is_empty() {
            return;
        }

        let mut indices: Vec<usize> = self.dirty.drain().collect();
        indices.sort_unstable_by(|a, b| b.cmp(a));

        for i in indices {
            let left = self.nodes[2 * i + 1];
            let right = self.nodes[2 * i + 2];
            self.nodes[i] = hash_internal(&left, &right);
        }
    }

    /// Perform a full rebuild of the node array from the BTreeMap leaves.
    ///
    /// Allocates a new flat array with power-of-two padding, fills the leaf
    /// layer, pads with `hash_leaf(&Hash::zero())`, and computes all internal
    /// nodes bottom-up. This is O(N log N) in the number of leaves (N hashes
    /// for leaves + N-1 hashes for internals).
    fn full_rebuild(&mut self) {
        let leaf_hashes: Vec<Hash> = self.leaves.values().copied().collect();
        let padded_count = leaf_hashes.len().next_power_of_two();
        let total_nodes = 2 * padded_count - 1;

        self.capacity = padded_count;
        self.nodes.clear();
        self.nodes.resize(total_nodes, Hash::zero());

        // Fill leaf layer.
        for (i, lh) in leaf_hashes.iter().enumerate() {
            self.nodes[padded_count - 1 + i] = hash_leaf(lh);
        }

        // Pad remaining leaf slots with hash_leaf(zero).
        let zero_leaf = hash_leaf(&Hash::zero());
        for i in leaf_hashes.len()..padded_count {
            self.nodes[padded_count - 1 + i] = zero_leaf;
        }

        // Build internal nodes bottom-up.
        for i in (0..padded_count - 1).rev() {
            let left = self.nodes[2 * i + 1];
            let right = self.nodes[2 * i + 2];
            self.nodes[i] = hash_internal(&left, &right);
        }
    }
}

/// Compute the root hash from a slice of leaf hashes.
///
/// Uses the same power-of-two padding and hash domain separators
/// as the crypto crate's MerkleTree. Retained for use in tests that
/// need a reference root computation independent of the StateTree.
#[cfg(test)]
fn compute_root(leaf_hashes: &[Hash]) -> Hash {
    if leaf_hashes.is_empty() {
        return Hash::zero();
    }

    let padded_count = leaf_hashes.len().next_power_of_two();
    let total_nodes = 2 * padded_count - 1;
    let mut nodes = vec![Hash::zero(); total_nodes];

    for (i, lh) in leaf_hashes.iter().enumerate() {
        nodes[padded_count - 1 + i] = hash_leaf(lh);
    }
    for i in leaf_hashes.len()..padded_count {
        nodes[padded_count - 1 + i] = hash_leaf(&Hash::zero());
    }

    for i in (0..padded_count - 1).rev() {
        let left = &nodes[2 * i + 1];
        let right = &nodes[2 * i + 2];
        nodes[i] = hash_internal(left, right);
    }

    nodes[0]
}

#[cfg(test)]
mod tests {
    use super::*;
    use nusantara_core::Account;
    use nusantara_crypto::hash;

    fn make_account(lamports: u64) -> Account {
        Account::new(lamports, hash(b"system"))
    }

    // ------------------------------------------------------------------
    // Original 13 tests (updated for &mut self on root/proof)
    // ------------------------------------------------------------------

    #[test]
    fn empty_tree_root_is_zero() {
        let mut tree = StateTree::new();
        assert_eq!(tree.root(), Hash::zero());
        assert!(tree.is_empty());
        assert_eq!(tree.len(), 0);
    }

    #[test]
    fn single_account_tree() {
        let mut tree = StateTree::new();
        let addr = hash(b"alice");
        let account = make_account(1000);
        tree.update(&[(addr, account.clone())]);

        assert_eq!(tree.len(), 1);
        let root = tree.root();
        assert_ne!(root, Hash::zero());

        // Proof should verify
        let proof = tree.proof(&addr).unwrap();
        assert!(StateTree::verify_proof(&root, &addr, &account, &proof));
    }

    #[test]
    fn deterministic_root() {
        let addr_a = hash(b"alice");
        let addr_b = hash(b"bob");
        let acc_a = make_account(1000);
        let acc_b = make_account(2000);

        let mut tree1 = StateTree::new();
        tree1.update(&[(addr_a, acc_a.clone()), (addr_b, acc_b.clone())]);

        let mut tree2 = StateTree::new();
        tree2.update(&[(addr_b, acc_b), (addr_a, acc_a)]);

        // Same accounts in different insertion order produce the same root
        // because BTreeMap sorts by key.
        assert_eq!(tree1.root(), tree2.root());
    }

    #[test]
    fn proof_verifies_for_all_accounts() {
        let addrs: Vec<Hash> = (0..10u8).map(|i| hash(&[i])).collect();
        let accounts: Vec<Account> = (0..10u64).map(|i| make_account(i * 100)).collect();

        let mut tree = StateTree::new();
        let deltas: Vec<(Hash, Account)> = addrs
            .iter()
            .zip(accounts.iter())
            .map(|(a, acc)| (*a, acc.clone()))
            .collect();
        tree.update(&deltas);

        let root = tree.root();

        for (addr, account) in addrs.iter().zip(accounts.iter()) {
            let proof = tree.proof(addr).unwrap();
            assert_eq!(proof.total_leaves, 10);
            assert!(
                StateTree::verify_proof(&root, addr, account, &proof),
                "proof failed for addr index in sorted order"
            );
        }
    }

    #[test]
    fn tampered_account_fails_verification() {
        let addr = hash(b"alice");
        let real_account = make_account(1000);
        let fake_account = make_account(9999);

        let mut tree = StateTree::new();
        tree.update(&[(addr, real_account)]);
        let root = tree.root();
        let proof = tree.proof(&addr).unwrap();

        assert!(!StateTree::verify_proof(
            &root,
            &addr,
            &fake_account,
            &proof
        ));
    }

    #[test]
    fn wrong_address_fails_verification() {
        let addr = hash(b"alice");
        let wrong_addr = hash(b"bob");
        let account = make_account(1000);

        let mut tree = StateTree::new();
        tree.update(&[(addr, account.clone())]);
        let root = tree.root();
        let proof = tree.proof(&addr).unwrap();

        assert!(!StateTree::verify_proof(
            &root,
            &wrong_addr,
            &account,
            &proof
        ));
    }

    #[test]
    fn incremental_update_matches_full_rebuild() {
        let addr_a = hash(b"alice");
        let addr_b = hash(b"bob");
        let addr_c = hash(b"carol");

        let acc_a = make_account(1000);
        let acc_b = make_account(2000);
        let acc_c = make_account(3000);

        // Build incrementally
        let mut incremental = StateTree::new();
        incremental.update(&[(addr_a, acc_a.clone()), (addr_b, acc_b.clone())]);
        incremental.update(&[(addr_c, acc_c.clone())]);

        // Build from scratch
        let mut full = StateTree::new();
        full.update(&[(addr_a, acc_a), (addr_b, acc_b), (addr_c, acc_c)]);

        assert_eq!(incremental.root(), full.root());
    }

    #[test]
    fn update_existing_account_changes_root() {
        let addr = hash(b"alice");
        let acc_v1 = make_account(1000);
        let acc_v2 = make_account(2000);

        let mut tree = StateTree::new();
        tree.update(&[(addr, acc_v1)]);
        let root1 = tree.root();

        tree.update(&[(addr, acc_v2)]);
        let root2 = tree.root();

        assert_ne!(root1, root2);
    }

    #[test]
    fn remove_account_changes_root() {
        let addr_a = hash(b"alice");
        let addr_b = hash(b"bob");
        let acc_a = make_account(1000);
        let acc_b = make_account(2000);

        let mut tree = StateTree::new();
        tree.update(&[(addr_a, acc_a.clone()), (addr_b, acc_b)]);
        let root_both = tree.root();

        tree.remove(&addr_b);
        let root_one = tree.root();

        assert_ne!(root_both, root_one);
        assert_eq!(tree.len(), 1);

        // Remaining account's proof still verifies
        let proof = tree.proof(&addr_a).unwrap();
        assert!(StateTree::verify_proof(&root_one, &addr_a, &acc_a, &proof));
    }

    #[test]
    fn remove_all_returns_to_zero_root() {
        let addr = hash(b"alice");
        let acc = make_account(1000);

        let mut tree = StateTree::new();
        tree.update(&[(addr, acc)]);
        assert_ne!(tree.root(), Hash::zero());

        tree.remove(&addr);
        assert_eq!(tree.root(), Hash::zero());
        assert!(tree.is_empty());
    }

    #[test]
    fn proof_for_missing_address_returns_none() {
        let addr = hash(b"alice");
        let missing = hash(b"bob");
        let acc = make_account(1000);

        let mut tree = StateTree::new();
        tree.update(&[(addr, acc)]);

        assert!(tree.proof(&missing).is_none());
    }

    #[test]
    fn proof_on_empty_tree_returns_none() {
        let mut tree = StateTree::new();
        assert!(tree.proof(&hash(b"alice")).is_none());
    }

    #[test]
    fn large_tree_proofs_verify() {
        let mut tree = StateTree::new();
        let mut deltas = Vec::new();
        for i in 0..100u64 {
            let addr = hash(&i.to_le_bytes());
            let acc = make_account(i * 1000);
            deltas.push((addr, acc));
        }
        tree.update(&deltas);

        let root = tree.root();
        for (addr, acc) in &deltas {
            let proof = tree.proof(addr).unwrap();
            assert!(StateTree::verify_proof(&root, addr, acc, &proof));
        }
    }

    #[test]
    fn non_power_of_two_leaf_count() {
        // 7 leaves -- not a power of two, tests padding behavior
        let mut tree = StateTree::new();
        let mut deltas = Vec::new();
        for i in 0..7u64 {
            let addr = hash(&i.to_le_bytes());
            let acc = make_account(i * 100);
            deltas.push((addr, acc));
        }
        tree.update(&deltas);

        let root = tree.root();
        for (addr, acc) in &deltas {
            let proof = tree.proof(addr).unwrap();
            assert!(StateTree::verify_proof(&root, addr, acc, &proof));
        }
    }

    // ------------------------------------------------------------------
    // New tests for incremental dirty-path tracking
    // ------------------------------------------------------------------

    /// Verify that the streaming `account_leaf_hash` produces the same result
    /// as the original `borsh::to_vec`-based approach.
    #[test]
    fn streaming_hash_matches_borsh_vec() {
        let addr = hash(b"streaming_test");
        let mut acc = Account::new(123_456_789, hash(b"owner"));
        acc.data = vec![1, 2, 3, 4, 5];
        acc.executable = true;
        acc.rent_epoch = 42;

        let streaming = account_leaf_hash(&addr, &acc);

        // Reference: original approach with borsh::to_vec
        let account_bytes = borsh::to_vec(&acc).expect("borsh");
        let reference = hashv(&[b"state_leaf", addr.as_bytes(), &account_bytes]);

        assert_eq!(streaming, reference);
    }

    /// Verify that the streaming hash works for accounts with empty data.
    #[test]
    fn streaming_hash_empty_data() {
        let addr = hash(b"empty_data");
        let acc = Account::new(100, hash(b"system"));

        let streaming = account_leaf_hash(&addr, &acc);
        let account_bytes = borsh::to_vec(&acc).expect("borsh");
        let reference = hashv(&[b"state_leaf", addr.as_bytes(), &account_bytes]);

        assert_eq!(streaming, reference);
    }

    /// After a full build (root() called), updating existing accounts should
    /// produce the same root as a from-scratch tree with the same final state.
    #[test]
    fn incremental_existing_update_same_root_as_full() {
        let addrs: Vec<Hash> = (0..5u8).map(|i| hash(&[i])).collect();
        let initial_accounts: Vec<Account> = (0..5u64).map(|i| make_account(i * 100)).collect();

        // Build and materialize the tree.
        let mut incremental = StateTree::new();
        let deltas: Vec<(Hash, Account)> = addrs
            .iter()
            .zip(initial_accounts.iter())
            .map(|(a, acc)| (*a, acc.clone()))
            .collect();
        incremental.update(&deltas);
        let _ = incremental.root(); // materialize node array

        // Update 2 existing accounts (no structural change).
        let updated_accounts = vec![
            (addrs[1], make_account(9999)),
            (addrs[3], make_account(7777)),
        ];
        incremental.update(&updated_accounts);

        // Build a reference tree from scratch with the final state.
        let mut reference = StateTree::new();
        let mut final_deltas = deltas;
        final_deltas[1].1 = make_account(9999);
        final_deltas[3].1 = make_account(7777);
        reference.update(&final_deltas);

        assert_eq!(incremental.root(), reference.root());

        // Verify the incremental path was used (structural_change should be false).
        assert!(!incremental.structural_change);
    }

    /// Build a large tree, update a small subset of existing accounts, and verify
    /// the root matches a full rebuild.
    #[test]
    fn large_tree_incremental_update() {
        let count = 10_000usize;
        let mut deltas: Vec<(Hash, Account)> = (0..count)
            .map(|i| {
                let addr = hash(&(i as u64).to_le_bytes());
                let acc = make_account(i as u64 * 100);
                (addr, acc)
            })
            .collect();

        // Build the tree and materialize.
        let mut tree = StateTree::new();
        tree.update(&deltas);
        let _ = tree.root();

        // Update 100 existing accounts.
        let updates: Vec<(Hash, Account)> = (0..100)
            .map(|i| {
                let addr = deltas[i * 100].0;
                let acc = make_account(999_999 + i as u64);
                (addr, acc)
            })
            .collect();
        tree.update(&updates);

        // Apply the same updates to the deltas vector and build a reference tree.
        for (i, (addr, acc)) in updates.iter().enumerate() {
            let idx = i * 100;
            assert_eq!(deltas[idx].0, *addr);
            deltas[idx].1 = acc.clone();
        }
        let mut reference = StateTree::new();
        reference.update(&deltas);

        assert_eq!(tree.root(), reference.root());
    }

    /// Mix of new-account adds and existing-account updates in a single batch.
    /// The structural change from the new account triggers a full rebuild, but
    /// the final root must still be correct.
    #[test]
    fn mixed_add_and_update() {
        let addr_a = hash(b"alpha");
        let addr_b = hash(b"beta");
        let acc_a = make_account(100);
        let acc_b = make_account(200);

        let mut tree = StateTree::new();
        tree.update(&[(addr_a, acc_a.clone())]);
        let _ = tree.root(); // materialize

        // Mixed batch: update existing addr_a + add new addr_b.
        let acc_a_v2 = make_account(999);
        tree.update(&[(addr_a, acc_a_v2.clone()), (addr_b, acc_b.clone())]);

        // Reference tree built from scratch.
        let mut reference = StateTree::new();
        reference.update(&[(addr_a, acc_a_v2.clone()), (addr_b, acc_b.clone())]);

        assert_eq!(tree.root(), reference.root());

        // Proofs should verify for both.
        let root = tree.root();
        let proof_a = tree.proof(&addr_a).unwrap();
        let proof_b = tree.proof(&addr_b).unwrap();
        assert!(StateTree::verify_proof(&root, &addr_a, &acc_a_v2, &proof_a));
        assert!(StateTree::verify_proof(&root, &addr_b, &acc_b, &proof_b));
    }

    /// Verify that the incremental root matches the standalone `compute_root`
    /// helper used by the original implementation.
    #[test]
    fn incremental_root_matches_compute_root() {
        let addrs: Vec<Hash> = (0..8u8).map(|i| hash(&[i + 100])).collect();
        let accounts: Vec<Account> = (0..8u64).map(|i| make_account(i * 50)).collect();

        let mut tree = StateTree::new();
        let deltas: Vec<(Hash, Account)> = addrs
            .iter()
            .zip(accounts.iter())
            .map(|(a, acc)| (*a, acc.clone()))
            .collect();
        tree.update(&deltas);
        let root = tree.root();

        // Compute reference root using the standalone function.
        let leaf_hashes: Vec<Hash> = tree.leaves.values().copied().collect();
        let reference = compute_root(&leaf_hashes);

        assert_eq!(root, reference);
    }

    /// Multiple incremental updates in sequence without structural changes.
    #[test]
    fn multiple_incremental_updates() {
        let addrs: Vec<Hash> = (0..4u8).map(|i| hash(&[i])).collect();
        let accounts: Vec<Account> = (0..4u64).map(|i| make_account(i * 100)).collect();

        let mut tree = StateTree::new();
        let deltas: Vec<(Hash, Account)> = addrs
            .iter()
            .zip(accounts.iter())
            .map(|(a, acc)| (*a, acc.clone()))
            .collect();
        tree.update(&deltas);
        let _ = tree.root(); // materialize

        // Round 1: update account 0
        tree.update(&[(addrs[0], make_account(1111))]);
        let root1 = tree.root();

        // Round 2: update account 2
        tree.update(&[(addrs[2], make_account(2222))]);
        let root2 = tree.root();

        assert_ne!(root1, root2);

        // Build reference with final state.
        let mut reference = StateTree::new();
        reference.update(&[
            (addrs[0], make_account(1111)),
            (addrs[1], make_account(100)),
            (addrs[2], make_account(2222)),
            (addrs[3], make_account(300)),
        ]);

        assert_eq!(root2, reference.root());
    }

    /// Verify that remove followed by re-add produces the correct root.
    #[test]
    fn remove_then_readd() {
        let addr_a = hash(b"addr_a");
        let addr_b = hash(b"addr_b");
        let acc_a = make_account(100);
        let acc_b = make_account(200);

        let mut tree = StateTree::new();
        tree.update(&[(addr_a, acc_a.clone()), (addr_b, acc_b.clone())]);
        let root_both = tree.root();

        // Remove b, then re-add b with same data.
        tree.remove(&addr_b);
        tree.update(&[(addr_b, acc_b)]);
        let root_readd = tree.root();

        assert_eq!(root_both, root_readd);
    }
}
