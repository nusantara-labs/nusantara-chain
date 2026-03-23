use std::cmp::Reverse;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};

use nusantara_core::Transaction;
use nusantara_core::native_token::const_parse_u64;
use nusantara_crypto::Hash;
use parking_lot::RwLock;
use tracing::instrument;

use crate::error::MempoolError;

/// Default maximum pool capacity, read from build-time config.
pub const DEFAULT_MAX_SIZE: u64 = const_parse_u64(env!("NUSA_POOL_MAX_SIZE"));

/// Default blockhash expiry window in slots.
pub const DEFAULT_EXPIRY_SLOT_WINDOW: u64 = const_parse_u64(env!("NUSA_POOL_EXPIRY_SLOT_WINDOW"));

/// Maximum transactions per payer account.
pub const MAX_TXS_PER_ACCOUNT: u64 = const_parse_u64(env!("NUSA_POOL_MAX_TXS_PER_ACCOUNT"));

/// Ordering key for the priority queue.
///
/// Transactions are sorted by:
///   1. Priority fee per compute unit (highest first via `Reverse`)
///   2. Insertion sequence (lowest first = FIFO tiebreaker)
///
/// `BTreeMap` sorts by `Ord`, so `Reverse<u64>` for priority gives us highest-first.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct MempoolKey {
    /// Negated priority: `Reverse(price)` so BTreeMap yields highest price first.
    neg_priority: Reverse<u64>,
    /// Monotonic insertion counter for FIFO ordering among equal-priority transactions.
    sequence: u64,
}

/// A transaction entry stored in the pool.
struct MempoolEntry {
    transaction: Transaction,
    tx_hash: Hash,
    /// Payer account (first account key) for per-sender limiting.
    payer: Hash,
}

/// A bounded, priority-ordered transaction mempool with deduplication and expiry.
///
/// Thread-safe: all public methods acquire internal locks. Locks are never held
/// across await points (this struct has no async methods).
///
/// Priority is extracted from the transaction's `SetComputeUnitPrice` instruction
/// via the runtime's `parse_compute_budget`. Transactions without a price instruction
/// default to priority 0.
///
/// When the pool is full, the lowest-priority transaction is evicted to make room
/// for a higher-priority incoming transaction. If the incoming transaction has
/// equal or lower priority than the current minimum, insertion is rejected.
pub struct Mempool {
    /// Priority-ordered map: highest priority (lowest `Reverse` value) comes first in iteration.
    pool: RwLock<BTreeMap<MempoolKey, MempoolEntry>>,
    /// Fast dedup lookup: tx_hash -> MempoolKey for O(1) duplicate detection and removal.
    dedup: RwLock<HashMap<Hash, MempoolKey>>,
    /// Per-payer transaction count for DoS protection.
    account_counts: RwLock<HashMap<Hash, usize>>,
    /// Monotonically increasing sequence counter for FIFO tiebreaking.
    sequence: AtomicU64,
    /// Maximum number of transactions the pool can hold.
    max_capacity: usize,
    /// Maximum transactions per payer account.
    max_txs_per_account: usize,
}

impl Mempool {
    /// Create a new mempool with the given maximum capacity.
    pub fn new(max_capacity: usize) -> Self {
        Self {
            pool: RwLock::new(BTreeMap::new()),
            dedup: RwLock::new(HashMap::with_capacity(max_capacity)),
            account_counts: RwLock::new(HashMap::new()),
            sequence: AtomicU64::new(0),
            max_capacity,
            max_txs_per_account: MAX_TXS_PER_ACCOUNT as usize,
        }
    }

    /// Insert a transaction into the mempool.
    ///
    /// Extracts the priority fee from the transaction's compute budget instructions.
    /// Rejects duplicates (by transaction hash). When the pool is at capacity,
    /// evicts the lowest-priority entry if the new transaction has strictly higher
    /// priority; otherwise returns `MempoolError::Full`.
    #[instrument(skip_all, name = "nusantara_mempool_insert")]
    pub fn insert(&self, tx: Transaction) -> Result<(), MempoolError> {
        let tx_hash = tx.hash();

        // Fast-path dedup check (read lock only)
        {
            let dedup = self.dedup.read();
            if dedup.contains_key(&tx_hash) {
                metrics::counter!("nusantara_mempool_duplicates").increment(1);
                return Err(MempoolError::DuplicateTransaction);
            }
        }

        // Extract payer (first account key)
        let payer = tx.message.account_keys[0];

        // Check per-sender limit
        {
            let counts = self.account_counts.read();
            if let Some(&count) = counts.get(&payer)
                && count >= self.max_txs_per_account
            {
                metrics::counter!("nusantara_mempool_account_limit_rejected").increment(1);
                return Err(MempoolError::AccountLimitExceeded {
                    payer,
                    limit: self.max_txs_per_account,
                });
            }
        }

        // Extract priority fee from compute budget instructions.
        // If parsing fails (no compute budget ix, or malformed), default to 0.
        let priority_fee_per_cu = extract_priority(&tx);

        let seq = self.sequence.fetch_add(1, Ordering::Relaxed);
        let key = MempoolKey {
            neg_priority: Reverse(priority_fee_per_cu),
            sequence: seq,
        };

        let entry = MempoolEntry {
            transaction: tx,
            tx_hash,
            payer,
        };

        // Acquire both locks for the mutation. We always acquire pool first, then dedup,
        // to maintain a consistent lock ordering and prevent deadlocks.
        let mut pool = self.pool.write();
        let mut dedup = self.dedup.write();
        let mut counts = self.account_counts.write();

        // Re-check dedup under write lock (another thread may have inserted concurrently)
        if dedup.contains_key(&tx_hash) {
            metrics::counter!("nusantara_mempool_duplicates").increment(1);
            return Err(MempoolError::DuplicateTransaction);
        }

        // Re-check per-sender limit under write lock
        if let Some(&count) = counts.get(&payer)
            && count >= self.max_txs_per_account
        {
            metrics::counter!("nusantara_mempool_account_limit_rejected").increment(1);
            return Err(MempoolError::AccountLimitExceeded {
                payer,
                limit: self.max_txs_per_account,
            });
        }

        if pool.len() >= self.max_capacity {
            // The last entry in the BTreeMap has the lowest priority (highest Reverse value, or
            // highest sequence among equal priority).
            if let Some((worst_key, _)) = pool.last_key_value() {
                if key >= *worst_key {
                    // New transaction is not better than the worst in pool
                    metrics::counter!("nusantara_mempool_rejected_full").increment(1);
                    return Err(MempoolError::Full {
                        capacity: self.max_capacity,
                    });
                }

                // Evict the lowest-priority transaction
                let worst_key = worst_key.clone();
                if let Some(evicted) = pool.remove(&worst_key) {
                    dedup.remove(&evicted.tx_hash);
                    if let Some(c) = counts.get_mut(&evicted.payer) {
                        *c = c.saturating_sub(1);
                        if *c == 0 {
                            counts.remove(&evicted.payer);
                        }
                    }
                    metrics::counter!("nusantara_mempool_evictions").increment(1);
                }
            }
        }

        dedup.insert(tx_hash, key.clone());
        *counts.entry(payer).or_insert(0) += 1;
        pool.insert(key, entry);

        metrics::gauge!("nusantara_mempool_size").set(pool.len() as f64);
        metrics::counter!("nusantara_mempool_inserts").increment(1);

        Ok(())
    }

    /// Drain up to `max` highest-priority transactions from the pool.
    ///
    /// Returns transactions ordered from highest to lowest priority.
    /// Drained transactions are removed from the pool and dedup index.
    #[instrument(skip_all, name = "nusantara_mempool_drain")]
    pub fn drain_by_priority(&self, max: usize) -> Vec<Transaction> {
        let mut pool = self.pool.write();
        let mut dedup = self.dedup.write();
        let mut counts = self.account_counts.write();

        let count = max.min(pool.len());
        let mut result = Vec::with_capacity(count);

        for _ in 0..count {
            // pop_first gives the entry with the smallest key = highest priority
            if let Some((_, entry)) = pool.pop_first() {
                dedup.remove(&entry.tx_hash);
                // Decrement per-sender count
                if let Some(c) = counts.get_mut(&entry.payer) {
                    *c = c.saturating_sub(1);
                    if *c == 0 {
                        counts.remove(&entry.payer);
                    }
                }
                result.push(entry.transaction);
            } else {
                break;
            }
        }

        metrics::gauge!("nusantara_mempool_size").set(pool.len() as f64);
        metrics::counter!("nusantara_mempool_drains").increment(result.len() as u64);

        result
    }

    /// Remove all transactions whose `recent_blockhash` is not in the given valid set.
    ///
    /// Uses `HashSet` for O(1) lookup instead of linear scan.
    /// This should be called periodically (e.g., every 10 slots) with the current
    /// valid blockhashes from the bank's slot hashes sysvar.
    #[instrument(skip_all, name = "nusantara_mempool_remove_expired")]
    pub fn remove_expired(&self, valid_blockhashes: &HashSet<Hash>) {
        let mut pool = self.pool.write();
        let mut dedup = self.dedup.write();
        let mut counts = self.account_counts.write();

        let before = pool.len();

        // Collect keys to remove (we cannot remove while iterating a BTreeMap)
        let expired_keys: Vec<MempoolKey> = pool
            .iter()
            .filter(|(_, entry)| {
                let bh = &entry.transaction.message.recent_blockhash;
                // Hash::zero() means "no expiry" (used by faucet/testing)
                *bh != Hash::zero() && !valid_blockhashes.contains(bh)
            })
            .map(|(key, _)| key.clone())
            .collect();

        for key in &expired_keys {
            if let Some(entry) = pool.remove(key) {
                dedup.remove(&entry.tx_hash);
                if let Some(c) = counts.get_mut(&entry.payer) {
                    *c = c.saturating_sub(1);
                    if *c == 0 {
                        counts.remove(&entry.payer);
                    }
                }
            }
        }

        let removed = before - pool.len();
        if removed > 0 {
            metrics::gauge!("nusantara_mempool_size").set(pool.len() as f64);
            metrics::counter!("nusantara_mempool_expired").increment(removed as u64);
            tracing::debug!(removed, "expired transactions removed from mempool");
        }
    }

    /// Returns the number of transactions currently in the pool.
    pub fn len(&self) -> usize {
        self.pool.read().len()
    }

    /// Returns `true` if the pool contains no transactions.
    pub fn is_empty(&self) -> bool {
        self.pool.read().is_empty()
    }

    /// Returns `true` if a transaction with the given hash is currently in the pool.
    pub fn contains(&self, tx_hash: &Hash) -> bool {
        self.dedup.read().contains_key(tx_hash)
    }
}

/// Extract the priority fee per compute unit from a transaction.
///
/// Parses the compute budget instructions to find `SetComputeUnitPrice`.
/// Returns 0 if no compute budget instruction is present or parsing fails.
fn extract_priority(tx: &Transaction) -> u64 {
    nusantara_runtime::compute_budget_parser::parse_compute_budget(&tx.message)
        .map(|budget| budget.compute_unit_price)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nusantara_compute_budget_program::set_compute_unit_price;
    use nusantara_core::Message;
    use nusantara_core::instruction::Instruction;
    use nusantara_core::program::SYSTEM_PROGRAM_ID;
    use nusantara_crypto::{Keypair, hash};

    /// Build a signed transaction with a given priority fee and blockhash.
    fn make_tx(priority: u64, blockhash: Hash) -> Transaction {
        let kp = Keypair::generate();
        let payer = kp.address();

        let transfer_ix = Instruction {
            program_id: *SYSTEM_PROGRAM_ID,
            accounts: vec![],
            data: borsh::to_vec(&nusantara_system_program::SystemInstruction::Transfer {
                lamports: 100,
            })
            .unwrap(),
        };

        let instructions = if priority > 0 {
            vec![set_compute_unit_price(priority), transfer_ix]
        } else {
            vec![transfer_ix]
        };

        let mut msg = Message::new(&instructions, &payer).unwrap();
        msg.recent_blockhash = blockhash;
        let mut tx = Transaction::new(msg);
        tx.sign(&[&kp]);
        tx
    }

    /// Build a signed transaction with a specific payer keypair.
    fn make_tx_with_payer(priority: u64, blockhash: Hash, kp: &Keypair) -> Transaction {
        let payer = kp.address();

        let transfer_ix = Instruction {
            program_id: *SYSTEM_PROGRAM_ID,
            accounts: vec![],
            data: borsh::to_vec(&nusantara_system_program::SystemInstruction::Transfer {
                lamports: 100,
            })
            .unwrap(),
        };

        let instructions = if priority > 0 {
            vec![set_compute_unit_price(priority), transfer_ix]
        } else {
            vec![transfer_ix]
        };

        let mut msg = Message::new(&instructions, &payer).unwrap();
        msg.recent_blockhash = blockhash;
        let mut tx = Transaction::new(msg);
        tx.sign(&[kp]);
        tx
    }

    #[test]
    fn config_values() {
        assert_eq!(DEFAULT_MAX_SIZE, 50_000);
        assert_eq!(DEFAULT_EXPIRY_SLOT_WINDOW, 150);
        assert_eq!(MAX_TXS_PER_ACCOUNT, 64);
    }

    #[test]
    fn insert_and_len() {
        let pool = Mempool::new(100);
        let bh = hash(b"blockhash");

        pool.insert(make_tx(0, bh)).unwrap();
        assert_eq!(pool.len(), 1);
        assert!(!pool.is_empty());
    }

    #[test]
    fn dedup_rejects_same_transaction() {
        let pool = Mempool::new(100);
        let bh = hash(b"blockhash");
        let tx = make_tx(0, bh);
        let tx_clone = tx.clone();

        pool.insert(tx).unwrap();
        let err = pool.insert(tx_clone).unwrap_err();
        assert!(matches!(err, MempoolError::DuplicateTransaction));
        assert_eq!(pool.len(), 1);
    }

    #[test]
    fn priority_ordering() {
        let pool = Mempool::new(100);
        let bh = hash(b"blockhash");

        // Insert low, medium, high priority
        pool.insert(make_tx(10, bh)).unwrap();
        pool.insert(make_tx(1000, bh)).unwrap();
        pool.insert(make_tx(100, bh)).unwrap();

        let drained = pool.drain_by_priority(3);
        assert_eq!(drained.len(), 3);

        // Extract priorities to verify ordering (highest first)
        let priorities: Vec<u64> = drained.iter().map(extract_priority).collect();
        assert_eq!(priorities, vec![1000, 100, 10]);
    }

    #[test]
    fn capacity_eviction() {
        let pool = Mempool::new(3);
        let bh = hash(b"blockhash");

        pool.insert(make_tx(10, bh)).unwrap();
        pool.insert(make_tx(20, bh)).unwrap();
        pool.insert(make_tx(30, bh)).unwrap();
        assert_eq!(pool.len(), 3);

        // Insert higher-priority tx: should evict the lowest (priority=10)
        pool.insert(make_tx(50, bh)).unwrap();
        assert_eq!(pool.len(), 3);

        let drained = pool.drain_by_priority(3);
        let priorities: Vec<u64> = drained.iter().map(extract_priority).collect();
        assert_eq!(priorities, vec![50, 30, 20]);
    }

    #[test]
    fn capacity_rejects_low_priority() {
        let pool = Mempool::new(2);
        let bh = hash(b"blockhash");

        pool.insert(make_tx(100, bh)).unwrap();
        pool.insert(make_tx(200, bh)).unwrap();

        // Lower priority than both existing entries
        let err = pool.insert(make_tx(50, bh)).unwrap_err();
        assert!(matches!(err, MempoolError::Full { capacity: 2 }));
        assert_eq!(pool.len(), 2);
    }

    #[test]
    fn drain_by_priority_respects_max() {
        let pool = Mempool::new(100);
        let bh = hash(b"blockhash");

        for i in 0..10 {
            pool.insert(make_tx(i * 10, bh)).unwrap();
        }

        let drained = pool.drain_by_priority(3);
        assert_eq!(drained.len(), 3);
        assert_eq!(pool.len(), 7);

        // Should get the top 3 priorities: 90, 80, 70
        let priorities: Vec<u64> = drained.iter().map(extract_priority).collect();
        assert_eq!(priorities, vec![90, 80, 70]);
    }

    #[test]
    fn drain_empty_pool() {
        let pool = Mempool::new(100);
        let drained = pool.drain_by_priority(10);
        assert!(drained.is_empty());
    }

    #[test]
    fn remove_expired() {
        let pool = Mempool::new(100);
        let bh_valid = hash(b"valid");
        let bh_expired = hash(b"expired");

        pool.insert(make_tx(10, bh_valid)).unwrap();
        pool.insert(make_tx(20, bh_expired)).unwrap();
        pool.insert(make_tx(30, bh_valid)).unwrap();
        assert_eq!(pool.len(), 3);

        pool.remove_expired(&HashSet::from([bh_valid]));
        assert_eq!(pool.len(), 2);

        // Only valid-blockhash transactions remain
        let drained = pool.drain_by_priority(10);
        for tx in &drained {
            assert_eq!(tx.message.recent_blockhash, bh_valid);
        }
    }

    #[test]
    fn remove_expired_empty_valid_set() {
        let pool = Mempool::new(100);
        let bh = hash(b"blockhash");

        pool.insert(make_tx(10, bh)).unwrap();
        pool.insert(make_tx(20, bh)).unwrap();

        // Empty valid set removes everything
        pool.remove_expired(&HashSet::new());
        assert!(pool.is_empty());
    }

    #[test]
    fn contains_after_insert() {
        let pool = Mempool::new(100);
        let bh = hash(b"blockhash");
        let tx = make_tx(0, bh);
        let tx_hash = tx.hash();

        assert!(!pool.contains(&tx_hash));
        pool.insert(tx).unwrap();
        assert!(pool.contains(&tx_hash));
    }

    #[test]
    fn contains_after_drain() {
        let pool = Mempool::new(100);
        let bh = hash(b"blockhash");
        let tx = make_tx(0, bh);
        let tx_hash = tx.hash();

        pool.insert(tx).unwrap();
        assert!(pool.contains(&tx_hash));

        pool.drain_by_priority(10);
        assert!(!pool.contains(&tx_hash));
    }

    #[test]
    fn is_empty_on_new_pool() {
        let pool = Mempool::new(100);
        assert!(pool.is_empty());
        assert_eq!(pool.len(), 0);
    }

    #[test]
    fn zero_priority_default() {
        let pool = Mempool::new(100);
        let bh = hash(b"blockhash");

        // Transaction with no compute budget instruction gets priority 0
        let tx = make_tx(0, bh);
        pool.insert(tx).unwrap();

        let drained = pool.drain_by_priority(1);
        assert_eq!(extract_priority(&drained[0]), 0);
    }

    #[test]
    fn per_sender_limit_enforced() {
        let pool = Mempool::new(1000);
        let bh = hash(b"blockhash");
        let kp = Keypair::generate();

        // Insert up to the limit
        for i in 0..MAX_TXS_PER_ACCOUNT {
            // Each tx needs a unique blockhash to avoid dedup
            let unique_bh = nusantara_crypto::hash(&i.to_le_bytes());
            pool.insert(make_tx_with_payer(i, unique_bh, &kp)).unwrap();
        }

        // The next one should be rejected
        let err = pool.insert(make_tx_with_payer(999, bh, &kp)).unwrap_err();
        assert!(matches!(err, MempoolError::AccountLimitExceeded { .. }));
    }

    #[test]
    fn per_sender_limit_independent_payers() {
        let pool = Mempool::new(1000);
        let kp1 = Keypair::generate();
        let kp2 = Keypair::generate();

        // Fill kp1 to the limit
        for i in 0..MAX_TXS_PER_ACCOUNT {
            let unique_bh = nusantara_crypto::hash(&i.to_le_bytes());
            pool.insert(make_tx_with_payer(i, unique_bh, &kp1)).unwrap();
        }

        // kp2 should still be able to insert
        let bh = hash(b"bh");
        assert!(pool.insert(make_tx_with_payer(0, bh, &kp2)).is_ok());
    }

    #[test]
    fn drain_allows_reinsertion() {
        let pool = Mempool::new(1000);
        let kp = Keypair::generate();

        // Fill to limit
        for i in 0..MAX_TXS_PER_ACCOUNT {
            let unique_bh = nusantara_crypto::hash(&i.to_le_bytes());
            pool.insert(make_tx_with_payer(i, unique_bh, &kp)).unwrap();
        }

        // Drain some
        pool.drain_by_priority(10);

        // Should be able to insert again
        let bh = hash(b"new_bh");
        assert!(pool.insert(make_tx_with_payer(500, bh, &kp)).is_ok());
    }
}
