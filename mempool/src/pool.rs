use std::cmp::Reverse;
use std::collections::{BTreeMap, HashMap, HashSet};

use metrics::{Counter, Gauge};
use nusantara_core::Transaction;
use nusantara_core::native_token::const_parse_u64;
use nusantara_crypto::Hash;
use parking_lot::RwLock;

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
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
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

/// All mutable pool state, guarded by a single RwLock to eliminate lock
/// ordering complexity and avoid holding multiple locks simultaneously.
struct MempoolInner {
    /// Priority-ordered map: highest priority (lowest `Reverse` value) comes first.
    pool: BTreeMap<MempoolKey, MempoolEntry>,
    /// Fast dedup lookup: tx_hash present = already in pool.
    dedup: HashSet<Hash>,
    /// Per-payer transaction count for DoS protection.
    account_counts: HashMap<Hash, usize>,
    /// Secondary index: blockhash -> set of MempoolKeys whose tx uses that blockhash.
    /// Enables O(expired) expiry removal instead of O(total).
    blockhash_index: HashMap<Hash, HashSet<MempoolKey>>,
    /// Monotonically increasing insertion counter. Lives inside the lock because
    /// it is always incremented under the write lock; a plain `u64` is sufficient.
    sequence: u64,
}

impl MempoolInner {
    fn new(capacity: usize) -> Self {
        Self {
            pool: BTreeMap::new(),
            dedup: HashSet::with_capacity(capacity),
            account_counts: HashMap::with_capacity(capacity),
            blockhash_index: HashMap::new(),
            sequence: 0,
        }
    }

    /// Verify internal structural invariants.
    ///
    /// Compiled in debug and test builds (zero cost in release). Asserts:
    /// - `pool.len() == dedup.len()`
    /// - `pool.len() == account_counts.values().sum()`
    /// - `pool.len() == blockhash_index total key count`
    #[cfg(debug_assertions)]
    fn assert_invariants(&self) {
        let pool_len = self.pool.len();
        let dedup_len = self.dedup.len();
        let count_sum: usize = self.account_counts.values().sum();
        let index_sum: usize = self.blockhash_index.values().map(|s| s.len()).sum();

        assert_eq!(
            pool_len, dedup_len,
            "pool.len ({pool_len}) != dedup.len ({dedup_len})"
        );
        assert_eq!(
            pool_len, count_sum,
            "pool.len ({pool_len}) != account_counts.sum ({count_sum})"
        );
        assert_eq!(
            pool_len, index_sum,
            "pool.len ({pool_len}) != blockhash_index.sum ({index_sum})"
        );
    }
}

/// Cached metric handles, obtained once at construction and reused on every call.
struct MempoolMetrics {
    gauge_size: Gauge,
    counter_inserts: Counter,
    counter_duplicates: Counter,
    counter_evictions: Counter,
    counter_rejected_full: Counter,
    counter_account_limit_rejected: Counter,
    counter_drains: Counter,
    counter_expired: Counter,
    counter_malformed: Counter,
}

impl MempoolMetrics {
    fn new() -> Self {
        Self {
            gauge_size: metrics::gauge!("nusantara_mempool_size"),
            counter_inserts: metrics::counter!("nusantara_mempool_inserts"),
            counter_duplicates: metrics::counter!("nusantara_mempool_duplicates"),
            counter_evictions: metrics::counter!("nusantara_mempool_evictions"),
            counter_rejected_full: metrics::counter!("nusantara_mempool_rejected_full"),
            counter_account_limit_rejected: metrics::counter!(
                "nusantara_mempool_account_limit_rejected"
            ),
            counter_drains: metrics::counter!("nusantara_mempool_drains"),
            counter_expired: metrics::counter!("nusantara_mempool_expired"),
            counter_malformed: metrics::counter!("nusantara_mempool_malformed"),
        }
    }
}

/// A bounded, priority-ordered transaction mempool with deduplication and expiry.
///
/// Thread-safe: all public methods acquire the single internal `RwLock`. Locks
/// are never held across `.await` points (this struct has no async methods).
///
/// Priority is extracted from the transaction's `SetComputeUnitPrice` instruction
/// via the runtime's `parse_compute_budget`. Transactions without a price instruction
/// default to priority 0.
///
/// When the pool is full, the lowest-priority transaction is evicted to make room
/// for a higher-priority incoming transaction. If the incoming transaction has
/// equal or lower priority than the current minimum, insertion is rejected.
///
/// # Preconditions
///
/// `max_capacity` must be > 0. Passing 0 will panic at construction.
pub struct Mempool {
    /// All mutable state behind one lock to prevent lock ordering bugs.
    inner: RwLock<MempoolInner>,
    /// Maximum number of transactions the pool can hold (immutable after init).
    max_capacity: usize,
    /// Maximum transactions per payer account (immutable after init).
    max_txs_per_account: usize,
    /// Pre-registered metric handles: avoids per-call handle lookup overhead.
    metrics: MempoolMetrics,
}

impl Mempool {
    /// Create a new mempool with the given maximum capacity.
    ///
    /// # Panics
    ///
    /// Panics if `max_capacity == 0`.
    pub fn new(max_capacity: usize) -> Self {
        assert!(max_capacity > 0, "Mempool::new: max_capacity must be > 0");
        Self {
            inner: RwLock::new(MempoolInner::new(max_capacity)),
            max_capacity,
            max_txs_per_account: MAX_TXS_PER_ACCOUNT as usize,
            metrics: MempoolMetrics::new(),
        }
    }

    /// Insert a transaction into the mempool.
    ///
    /// Validates that the transaction has at least one account key (the payer) and
    /// that the signatures vector length matches the declared signer count.
    /// Rejects duplicates (by transaction hash). When the pool is at capacity,
    /// evicts the lowest-priority entry if the new transaction has strictly higher
    /// priority; otherwise returns `MempoolError::Full`.
    pub fn insert(&self, tx: Transaction) -> Result<(), MempoolError> {
        // Validate: payer must exist as the first account key.
        if tx.message.account_keys.is_empty() {
            self.metrics.counter_malformed.increment(1);
            return Err(MempoolError::Malformed {
                reason: "account_keys is empty; payer missing",
            });
        }

        // Validate: signatures count must match num_required_signatures.
        let required_sigs = tx.message.num_required_signatures as usize;
        if tx.signatures.len() != required_sigs {
            self.metrics.counter_malformed.increment(1);
            return Err(MempoolError::Malformed {
                reason: "signatures length does not match num_required_signatures",
            });
        }

        let tx_hash = tx.hash();
        let payer = tx.message.account_keys[0];

        // Fast-path dedup check (read lock only).
        {
            let inner = self.inner.read();
            if inner.dedup.contains(&tx_hash) {
                self.metrics.counter_duplicates.increment(1);
                return Err(MempoolError::DuplicateTransaction);
            }
            if let Some(&count) = inner.account_counts.get(&payer)
                && count >= self.max_txs_per_account
            {
                self.metrics.counter_account_limit_rejected.increment(1);
                return Err(MempoolError::AccountLimitExceeded {
                    payer,
                    limit: self.max_txs_per_account,
                });
            }
        }

        // Extract priority fee from compute budget instructions.
        // If parsing fails (no compute budget ix, or malformed), default to 0.
        let priority_fee_per_cu = extract_priority(&tx);
        let recent_blockhash = tx.message.recent_blockhash;

        // Acquire sequence number only AFTER all pre-checks pass to keep FIFO
        // ordering tight: a rejected tx must not consume a sequence slot.
        let mut inner = self.inner.write();

        // Re-check dedup under write lock (another thread may have inserted concurrently).
        if inner.dedup.contains(&tx_hash) {
            self.metrics.counter_duplicates.increment(1);
            return Err(MempoolError::DuplicateTransaction);
        }

        // Re-check per-sender limit under write lock.
        if let Some(&count) = inner.account_counts.get(&payer)
            && count >= self.max_txs_per_account
        {
            self.metrics.counter_account_limit_rejected.increment(1);
            return Err(MempoolError::AccountLimitExceeded {
                payer,
                limit: self.max_txs_per_account,
            });
        }

        if inner.pool.len() >= self.max_capacity {
            // The last entry in the BTreeMap has the lowest priority (highest
            // `Reverse` value, or highest sequence among equal priority).
            //
            // We compare on priority alone BEFORE allocating a sequence number so
            // that a rejected insertion never burns a sequence slot. A new tx with
            // equal priority to the current worst would also lose (its sequence
            // would be larger, making its key sort after the worst's key), so we
            // reject `new_priority <= worst_priority` as a single condition.
            let worst_priority = inner
                .pool
                .last_key_value()
                .map(|(k, _)| k.neg_priority)
                // Pool is non-empty (len >= max_capacity > 0), so this is unreachable.
                .expect("pool non-empty: last_key_value must return Some");

            if Reverse(priority_fee_per_cu) >= worst_priority {
                // Incoming priority is equal or lower — reject without touching sequence.
                self.metrics.counter_rejected_full.increment(1);
                return Err(MempoolError::Full {
                    capacity: self.max_capacity,
                });
            }

            // Incoming tx has strictly higher priority: evict the worst entry first,
            // then allocate the sequence number so no slot is consumed on failure.
            let worst_key = inner
                .pool
                .last_key_value()
                .map(|(k, _)| k.clone())
                .expect("pool non-empty: last_key_value must return Some");

            let evicted = inner
                .pool
                .remove(&worst_key)
                .expect("worst_key invariant: just observed via last_key_value");
            inner.dedup.remove(&evicted.tx_hash);
            // account_counts invariant: payer must have an entry.
            let c = inner
                .account_counts
                .get_mut(&evicted.payer)
                .expect("account_counts invariant: evicted payer must be present");
            *c -= 1;
            if *c == 0 {
                inner.account_counts.remove(&evicted.payer);
            }
            // Clean up the blockhash secondary index.
            let bh = evicted.transaction.message.recent_blockhash;
            if let Some(bucket) = inner.blockhash_index.get_mut(&bh) {
                bucket.remove(&worst_key);
                if bucket.is_empty() {
                    inner.blockhash_index.remove(&bh);
                }
            }
            self.metrics.counter_evictions.increment(1);
        }

        // Allocate the sequence number only after all rejection paths are cleared.
        // This guarantees that a rejected insertion never consumes a sequence slot,
        // keeping FIFO ordering tight among accepted transactions.
        let seq = inner.sequence;
        inner.sequence += 1;
        let key = MempoolKey {
            neg_priority: Reverse(priority_fee_per_cu),
            sequence: seq,
        };

        // Insert into all secondary structures.
        inner.dedup.insert(tx_hash);
        *inner.account_counts.entry(payer).or_insert(0) += 1;
        inner
            .blockhash_index
            .entry(recent_blockhash)
            .or_default()
            .insert(key.clone());
        inner.pool.insert(
            key,
            MempoolEntry {
                transaction: tx,
                tx_hash,
                payer,
            },
        );

        self.metrics.gauge_size.set(inner.pool.len() as f64);
        self.metrics.counter_inserts.increment(1);

        #[cfg(debug_assertions)]
        inner.assert_invariants();

        Ok(())
    }

    /// Drain up to `max` highest-priority transactions from the pool.
    ///
    /// Returns transactions ordered from highest to lowest priority.
    /// Drained transactions are removed from the pool and all secondary indexes.
    pub fn drain_by_priority(&self, max: usize) -> Vec<Transaction> {
        let mut inner = self.inner.write();

        let count = max.min(inner.pool.len());
        let mut result = Vec::with_capacity(count);

        for _ in 0..count {
            // pop_first gives the entry with the smallest key = highest priority.
            if let Some((key, entry)) = inner.pool.pop_first() {
                inner.dedup.remove(&entry.tx_hash);
                // account_counts invariant: payer must have an entry.
                let c = inner
                    .account_counts
                    .get_mut(&entry.payer)
                    .expect("account_counts invariant: drained payer must be present");
                *c -= 1;
                if *c == 0 {
                    inner.account_counts.remove(&entry.payer);
                }
                // Clean up the blockhash secondary index.
                let bh = entry.transaction.message.recent_blockhash;
                if let Some(bucket) = inner.blockhash_index.get_mut(&bh) {
                    bucket.remove(&key);
                    if bucket.is_empty() {
                        inner.blockhash_index.remove(&bh);
                    }
                }
                result.push(entry.transaction);
            } else {
                break;
            }
        }

        self.metrics.gauge_size.set(inner.pool.len() as f64);
        self.metrics.counter_drains.increment(result.len() as u64);

        #[cfg(debug_assertions)]
        inner.assert_invariants();

        result
    }

    /// Remove all transactions whose `recent_blockhash` is not in the given valid set.
    ///
    /// Uses the blockhash secondary index so work is proportional to the number of
    /// expired entries rather than the total pool size.
    ///
    /// This should be called periodically (e.g., every 10 slots) with the current
    /// valid blockhashes from the bank's slot hashes sysvar.
    pub fn remove_expired(&self, valid_blockhashes: &HashSet<Hash>) {
        let mut inner = self.inner.write();

        // Collect all blockhashes in the index that are NOT valid.
        let expired_blockhashes: Vec<Hash> = inner
            .blockhash_index
            .keys()
            .filter(|bh| !valid_blockhashes.contains(*bh))
            .copied()
            .collect();

        let mut removed: usize = 0;

        for bh in &expired_blockhashes {
            if let Some(bucket) = inner.blockhash_index.remove(bh) {
                for key in &bucket {
                    if let Some(entry) = inner.pool.remove(key) {
                        inner.dedup.remove(&entry.tx_hash);
                        // account_counts invariant: payer must have an entry.
                        let c = inner
                            .account_counts
                            .get_mut(&entry.payer)
                            .expect("account_counts invariant: expired payer must be present");
                        *c -= 1;
                        if *c == 0 {
                            inner.account_counts.remove(&entry.payer);
                        }
                        removed += 1;
                    }
                }
            }
        }

        if removed > 0 {
            self.metrics.gauge_size.set(inner.pool.len() as f64);
            self.metrics.counter_expired.increment(removed as u64);
            tracing::debug!(removed, "expired transactions removed from mempool");
        }

        #[cfg(debug_assertions)]
        inner.assert_invariants();
    }

    /// Returns the number of transactions currently in the pool.
    pub fn len(&self) -> usize {
        self.inner.read().pool.len()
    }

    /// Returns `true` if the pool contains no transactions.
    pub fn is_empty(&self) -> bool {
        self.inner.read().pool.is_empty()
    }

    /// Returns `true` if a transaction with the given hash is currently in the pool.
    pub fn contains(&self, tx_hash: &Hash) -> bool {
        self.inner.read().dedup.contains(tx_hash)
    }

    /// Expose invariant checking to test code. Delegates to `MempoolInner::assert_invariants`,
    /// which is compiled whenever `debug_assertions` is active (including all test builds).
    #[cfg(test)]
    pub fn assert_invariants(&self) {
        self.inner.read().assert_invariants();
    }
}

/// Extract the priority fee per compute unit from a transaction.
///
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

    /// Build a Transaction whose message has no account_keys (hostile input).
    fn make_empty_account_keys_tx() -> Transaction {
        // Craft a message with no account_keys and zero required signatures so
        // the signature-count check passes before the account_keys check fires.
        let msg = Message {
            num_required_signatures: 0,
            num_readonly_signed: 0,
            num_readonly_unsigned: 0,
            account_keys: vec![],
            recent_blockhash: hash(b"any"),
            instructions: vec![],
        };
        Transaction::new(msg)
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
        pool.assert_invariants();
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
        pool.assert_invariants();
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
        pool.assert_invariants();
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
        pool.assert_invariants();
    }

    /// Rejected insertions (Full, Duplicate, AccountLimit) must NOT advance the
    /// sequence counter. After a batch of successful inserts, FIFO order must be
    /// gapless — no holes from burned sequence numbers.
    #[test]
    fn rejected_insert_does_not_advance_sequence() {
        let pool = Mempool::new(2);
        let bh = hash(b"blockhash");

        // Fill pool with two entries at priority 100 and 200.
        pool.insert(make_tx(100, bh)).unwrap();
        pool.insert(make_tx(200, bh)).unwrap();

        // Record sequence before the rejected call.
        let seq_before = pool.inner.read().sequence;

        // This insert is rejected (Full, priority 50 < worst priority 100).
        let err = pool.insert(make_tx(50, bh)).unwrap_err();
        assert!(matches!(err, MempoolError::Full { .. }));

        // Sequence must not have advanced.
        let seq_after = pool.inner.read().sequence;
        assert_eq!(
            seq_before, seq_after,
            "sequence advanced on rejected Full insert: before={seq_before}, after={seq_after}"
        );
        pool.assert_invariants();
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
        pool.assert_invariants();
    }

    #[test]
    fn drain_empty_pool() {
        let pool = Mempool::new(100);
        let drained = pool.drain_by_priority(10);
        assert!(drained.is_empty());
        pool.assert_invariants();
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
        pool.assert_invariants();

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
        pool.assert_invariants();
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
        pool.assert_invariants();
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
        pool.assert_invariants();
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
        pool.assert_invariants();
    }

    // ---- New tests per the review ----

    #[test]
    #[should_panic(expected = "max_capacity must be > 0")]
    fn new_with_zero_capacity_panics() {
        let _ = Mempool::new(0);
    }

    #[test]
    fn empty_account_keys_returns_malformed() {
        let pool = Mempool::new(100);
        let tx = make_empty_account_keys_tx();
        let err = pool.insert(tx).unwrap_err();
        assert!(
            matches!(err, MempoolError::Malformed { .. }),
            "expected Malformed, got {err:?}"
        );
        // Pool must be empty — no panic occurred.
        assert!(pool.is_empty());
    }

    #[test]
    fn signature_count_mismatch_returns_malformed() {
        let pool = Mempool::new(100);

        // Build a normally-signed tx (1 signature, num_required_signatures=1),
        // then tamper: bump num_required_signatures to 2 without adding a sig.
        let bh = hash(b"bh");
        let kp = Keypair::generate();
        let mut tx = make_tx_with_payer(0, bh, &kp);
        assert_eq!(tx.signatures.len(), 1);
        // Declare 2 required signers — signatures vec still has only 1.
        tx.message.num_required_signatures = 2;

        let err = pool.insert(tx).unwrap_err();
        assert!(matches!(err, MempoolError::Malformed { .. }));
    }

    #[test]
    fn zero_blockhash_is_subject_to_expiry() {
        let pool = Mempool::new(100);
        let zero_bh = Hash::zero();

        // A tx with Hash::zero() blockhash — previously exempt from expiry.
        // Now it must be treated like any other blockhash.
        let kp = Keypair::generate();
        let tx = make_tx_with_payer(0, zero_bh, &kp);
        pool.insert(tx).unwrap();
        assert_eq!(pool.len(), 1);

        // Expire with an empty valid set — should remove the zero-blockhash tx too.
        pool.remove_expired(&HashSet::new());
        assert!(pool.is_empty(), "Hash::zero() tx must expire like any other");
        pool.assert_invariants();
    }

    #[test]
    fn remove_expired_blockhash_index_correctness() {
        let pool = Mempool::new(100);
        let bh_a = hash(b"a");
        let bh_b = hash(b"b");
        let bh_c = hash(b"c");

        // Insert 2 txs per blockhash.
        for i in 0u64..2 {
            pool.insert(make_tx(i * 10 + 1, bh_a)).unwrap();
            pool.insert(make_tx(i * 10 + 2, bh_b)).unwrap();
            pool.insert(make_tx(i * 10 + 3, bh_c)).unwrap();
        }
        assert_eq!(pool.len(), 6);
        pool.assert_invariants();

        // Expire only bh_b.
        pool.remove_expired(&HashSet::from([bh_a, bh_c]));

        // Only the 2 bh_b txs should be removed.
        assert_eq!(pool.len(), 4);
        pool.assert_invariants();

        // Verify remaining txs have only bh_a or bh_c.
        let remaining = pool.drain_by_priority(10);
        for tx in &remaining {
            assert_ne!(
                tx.message.recent_blockhash, bh_b,
                "expired blockhash found after remove_expired"
            );
        }
    }

    /// Regression: two txs from the SAME payer sharing the SAME blockhash must both
    /// decrement `account_counts` to zero on expiry — no double-decrement and no
    /// residual entry left at count 1.
    #[test]
    fn remove_expired_same_payer_same_blockhash() {
        let pool = Mempool::new(100);
        let bh_expire = hash(b"expire");
        let bh_keep = hash(b"keep");
        let kp = Keypair::generate();

        // Two txs, same payer, same expiring blockhash.
        // They differ in priority so their tx hashes differ (different instructions).
        pool.insert(make_tx_with_payer(10, bh_expire, &kp)).unwrap();
        pool.insert(make_tx_with_payer(20, bh_expire, &kp)).unwrap();
        // One tx from the same payer on a valid blockhash, to confirm it survives.
        pool.insert(make_tx_with_payer(30, bh_keep, &kp)).unwrap();
        assert_eq!(pool.len(), 3);
        pool.assert_invariants();

        // Expire the shared blockhash. Both bh_expire txs should be removed.
        pool.remove_expired(&HashSet::from([bh_keep]));
        assert_eq!(pool.len(), 1, "only the bh_keep tx should survive");
        pool.assert_invariants();

        // The surviving tx still belongs to the same payer: account_counts must
        // show exactly 1 for this payer — not 0 (under-counted) or 3 (not decremented).
        {
            let inner = pool.inner.read();
            let payer = kp.address();
            let count = inner.account_counts.get(&payer).copied().unwrap_or(0);
            assert_eq!(
                count, 1,
                "account_counts for payer must be 1 after expiring 2 of 3 txs, got {count}"
            );
        }

        // Expire everything: account_counts must not contain the payer at all.
        pool.remove_expired(&HashSet::new());
        assert!(pool.is_empty());
        pool.assert_invariants();
        {
            let inner = pool.inner.read();
            let payer = kp.address();
            assert!(
                !inner.account_counts.contains_key(&payer),
                "account_counts must not contain payer after all txs expired"
            );
        }
    }

    #[test]
    fn concurrent_stress_invariants() {
        use std::sync::Arc;
        use std::thread;

        let pool = Arc::new(Mempool::new(500));

        // Spawn 4 inserter threads, each inserting 250 txs.
        let handles: Vec<_> = (0..4)
            .map(|t| {
                let pool = Arc::clone(&pool);
                thread::spawn(move || {
                    for i in 0u64..250 {
                        // Use thread-unique + index-unique blockhash to avoid dedup.
                        let unique_bh = nusantara_crypto::hash(
                            &((t as u64 * 1000 + i).to_le_bytes()),
                        );
                        let _ = pool.insert(make_tx(i, unique_bh));
                    }
                })
            })
            .collect();

        for h in handles {
            h.join().unwrap();
        }

        // Pool must not exceed capacity.
        assert!(pool.len() <= 500);

        // Drain half.
        let drained = pool.drain_by_priority(pool.len() / 2);
        assert!(!drained.is_empty());

        // Structural invariants must hold after concurrent inserts + partial drain.
        pool.assert_invariants();
    }

    /// Same concurrent stress test driven from the Tokio runtime to confirm that
    /// parking_lot's sync primitives compose correctly with async task scheduling.
    /// `spawn_blocking` is used because `insert` is synchronous and may momentarily
    /// block on the write lock — it must not be called directly on an async task.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_stress_invariants_tokio() {
        use std::sync::Arc;

        let pool = Arc::new(Mempool::new(500));

        // Spawn 4 blocking tasks, each inserting 250 transactions.
        let handles: Vec<_> = (0..4u64)
            .map(|t| {
                let pool = Arc::clone(&pool);
                tokio::task::spawn_blocking(move || {
                    for i in 0u64..250 {
                        // Use task-unique + index-unique blockhash to avoid dedup.
                        let unique_bh =
                            nusantara_crypto::hash(&((t * 1000 + i).to_le_bytes()));
                        let _ = pool.insert(make_tx(i, unique_bh));
                    }
                })
            })
            .collect();

        for h in handles {
            h.await.expect("blocking task must not panic");
        }

        // Pool must not exceed capacity.
        assert!(pool.len() <= 500);

        // Drain half via a blocking task to avoid holding the lock on the async thread.
        let pool2 = Arc::clone(&pool);
        let drained = tokio::task::spawn_blocking(move || pool2.drain_by_priority(pool2.len() / 2))
            .await
            .expect("drain blocking task must not panic");
        assert!(!drained.is_empty());

        // Structural invariants must hold after concurrent inserts + partial drain.
        pool.assert_invariants();
    }
}
