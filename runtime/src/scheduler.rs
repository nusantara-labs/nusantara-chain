//! Transaction scheduler for Sealevel-style parallel execution.
//!
//! Groups non-conflicting transactions into parallel batches by analyzing
//! the read/write account sets of each transaction. Two transactions conflict
//! when they both write to the same account, or one writes and the other reads
//! the same account. Read-read access to the same account is always safe.
//!
//! The scheduler produces batches in the original transaction order: within
//! each batch the `tx_indices` refer to positions in the original input slice.
//! This guarantees deterministic execution when results are committed in
//! original-index order.

use std::collections::HashSet;

use nusantara_core::Transaction;
use nusantara_crypto::Hash;

/// The set of accounts a transaction reads from and writes to.
pub struct AccessSet {
    pub writable: Vec<Hash>,
    pub readable: Vec<Hash>,
}

/// A batch of non-conflicting transactions that can execute in parallel.
pub struct ParallelBatch {
    /// Indices into the original transaction slice, preserving input order.
    pub tx_indices: Vec<usize>,
}

/// Schedules transactions into parallel batches based on read/write conflicts.
pub struct TransactionScheduler;

impl TransactionScheduler {
    /// Extract the read/write account sets from a transaction's message.
    ///
    /// Uses the positional `is_writable()` logic from [`nusantara_core::Message`]:
    /// accounts in writable positions are added to the writable set, all others
    /// (including program accounts) are added to the readable set.
    pub fn extract_access_set(tx: &Transaction) -> AccessSet {
        let msg = &tx.message;
        let mut writable = Vec::new();
        let mut readable = Vec::new();

        for (i, key) in msg.account_keys.iter().enumerate() {
            if msg.is_writable(i) {
                writable.push(*key);
            } else {
                readable.push(*key);
            }
        }

        AccessSet { writable, readable }
    }

    /// Group transactions into batches where no two txs in the same batch
    /// conflict on account access.
    ///
    /// A conflict occurs when:
    /// - Two transactions both write to the same account (write-write).
    /// - One transaction writes and another reads the same account (write-read
    ///   or read-write).
    ///
    /// Read-read access to the same account does **not** constitute a conflict.
    ///
    /// When a conflict is detected, the current batch is flushed and a new one
    /// is started. This greedy approach preserves original ordering and produces
    /// deterministic batches for the same input.
    pub fn schedule(transactions: &[Transaction]) -> Vec<ParallelBatch> {
        if transactions.is_empty() {
            return vec![];
        }

        let mut batches: Vec<ParallelBatch> = Vec::new();
        let mut current_batch: Vec<usize> = Vec::new();
        let mut write_locked: HashSet<Hash> = HashSet::new();
        let mut read_locked: HashSet<Hash> = HashSet::new();

        for (i, tx) in transactions.iter().enumerate() {
            let access = Self::extract_access_set(tx);

            // A conflict exists if:
            // - Any of our writable accounts is already write-locked or read-locked
            // - Any of our readable accounts is already write-locked
            let conflicts = access
                .writable
                .iter()
                .any(|a| write_locked.contains(a) || read_locked.contains(a))
                || access.readable.iter().any(|a| write_locked.contains(a));

            if conflicts {
                // Flush the current batch and start a new one
                batches.push(ParallelBatch {
                    tx_indices: std::mem::take(&mut current_batch),
                });
                write_locked.clear();
                read_locked.clear();
            }

            current_batch.push(i);
            write_locked.extend(access.writable);
            read_locked.extend(access.readable);
        }

        // Flush the final batch
        if !current_batch.is_empty() {
            batches.push(ParallelBatch {
                tx_indices: current_batch,
            });
        }

        batches
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nusantara_core::Message;
    use nusantara_core::instruction::{AccountMeta, Instruction};
    use nusantara_crypto::{Keypair, hash};

    /// Helper: build a signed transfer transaction from `from_kp` to `to`.
    fn transfer_tx(from_kp: &Keypair, to: Hash, amount: u64) -> Transaction {
        let from = from_kp.address();
        let ix = nusantara_system_program::transfer(&from, &to, amount);
        let msg = Message::new(&[ix], &from).unwrap();
        let mut tx = Transaction::new(msg);
        tx.sign(&[from_kp]);
        tx
    }

    /// Helper: build a transaction that reads `readonly_key` and writes `writable_key`.
    fn custom_tx(payer_kp: &Keypair, writable_key: Hash, readonly_key: Hash) -> Transaction {
        let payer = payer_kp.address();
        let program = hash(b"custom_program");
        let ix = Instruction {
            program_id: program,
            accounts: vec![
                AccountMeta::new(writable_key, false),
                AccountMeta::new_readonly(readonly_key, false),
            ],
            data: vec![1],
        };
        let msg = Message::new(&[ix], &payer).unwrap();
        let mut tx = Transaction::new(msg);
        tx.sign(&[payer_kp]);
        tx
    }

    #[test]
    fn empty_transactions() {
        let batches = TransactionScheduler::schedule(&[]);
        assert!(batches.is_empty());
    }

    #[test]
    fn single_transaction() {
        let kp = Keypair::generate();
        let to = hash(b"bob");
        let tx = transfer_tx(&kp, to, 100);

        let batches = TransactionScheduler::schedule(&[tx]);
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].tx_indices, vec![0]);
    }

    #[test]
    fn non_conflicting_independent_transfers() {
        // Alice -> Bob and Carol -> Dave: completely independent accounts
        let alice_kp = Keypair::generate();
        let carol_kp = Keypair::generate();
        let bob = hash(b"bob");
        let dave = hash(b"dave");

        let tx1 = transfer_tx(&alice_kp, bob, 100);
        let tx2 = transfer_tx(&carol_kp, dave, 200);

        let batches = TransactionScheduler::schedule(&[tx1, tx2]);
        // Both should be in the same batch (no conflict)
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].tx_indices, vec![0, 1]);
    }

    #[test]
    fn write_write_conflict_same_payer() {
        // Same payer (write-write on payer account) -> must split
        let alice_kp = Keypair::generate();
        let bob = hash(b"bob");
        let carol = hash(b"carol");

        let tx1 = transfer_tx(&alice_kp, bob, 100);
        let tx2 = transfer_tx(&alice_kp, carol, 200);

        let batches = TransactionScheduler::schedule(&[tx1, tx2]);
        assert_eq!(batches.len(), 2);
        assert_eq!(batches[0].tx_indices, vec![0]);
        assert_eq!(batches[1].tx_indices, vec![1]);
    }

    #[test]
    fn write_write_conflict_same_destination() {
        // Different payers but same writable destination -> conflict
        let alice_kp = Keypair::generate();
        let carol_kp = Keypair::generate();
        let bob = hash(b"bob");

        let tx1 = transfer_tx(&alice_kp, bob, 100);
        let tx2 = transfer_tx(&carol_kp, bob, 200);

        let batches = TransactionScheduler::schedule(&[tx1, tx2]);
        // Both write to bob -> must split
        assert_eq!(batches.len(), 2);
        assert_eq!(batches[0].tx_indices, vec![0]);
        assert_eq!(batches[1].tx_indices, vec![1]);
    }

    #[test]
    fn read_read_no_conflict() {
        // Two transactions that both READ the same readonly account should batch together
        let alice_kp = Keypair::generate();
        let carol_kp = Keypair::generate();
        let shared_readonly = hash(b"shared_data");
        let writable_a = hash(b"writable_a");
        let writable_c = hash(b"writable_c");

        let tx1 = custom_tx(&alice_kp, writable_a, shared_readonly);
        let tx2 = custom_tx(&carol_kp, writable_c, shared_readonly);

        let batches = TransactionScheduler::schedule(&[tx1, tx2]);
        // Read-read is safe: both in one batch
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].tx_indices, vec![0, 1]);
    }

    #[test]
    fn write_read_conflict() {
        // tx1 writes to account X, tx2 reads account X -> conflict
        let alice_kp = Keypair::generate();
        let carol_kp = Keypair::generate();
        let shared_account = hash(b"shared");
        let writable_c = hash(b"writable_c");

        // tx1: writes shared_account
        let tx1 = custom_tx(&alice_kp, shared_account, hash(b"other_readonly"));
        // tx2: reads shared_account
        let tx2 = custom_tx(&carol_kp, writable_c, shared_account);

        let batches = TransactionScheduler::schedule(&[tx1, tx2]);
        assert_eq!(batches.len(), 2);
    }

    #[test]
    fn all_conflicting_sequential() {
        // Three transactions all touching the same writable account
        let alice_kp = Keypair::generate();
        let bob = hash(b"bob");

        let tx1 = transfer_tx(&alice_kp, bob, 100);
        let tx2 = transfer_tx(&alice_kp, bob, 200);
        let tx3 = transfer_tx(&alice_kp, bob, 300);

        let batches = TransactionScheduler::schedule(&[tx1, tx2, tx3]);
        // Each in its own batch
        assert_eq!(batches.len(), 3);
        assert_eq!(batches[0].tx_indices, vec![0]);
        assert_eq!(batches[1].tx_indices, vec![1]);
        assert_eq!(batches[2].tx_indices, vec![2]);
    }

    #[test]
    fn mixed_conflict_and_independent() {
        // tx0: Alice -> Bob (writes Alice, Bob)
        // tx1: Carol -> Dave (writes Carol, Dave) -- no conflict with tx0
        // tx2: Alice -> Eve (writes Alice, Eve) -- conflicts with tx0 (Alice)
        let alice_kp = Keypair::generate();
        let carol_kp = Keypair::generate();
        let bob = hash(b"bob");
        let dave = hash(b"dave");
        let eve = hash(b"eve");

        let tx0 = transfer_tx(&alice_kp, bob, 100);
        let tx1 = transfer_tx(&carol_kp, dave, 200);
        let tx2 = transfer_tx(&alice_kp, eve, 300);

        let batches = TransactionScheduler::schedule(&[tx0, tx1, tx2]);
        // tx0 and tx1 can batch together; tx2 conflicts with tx0
        assert_eq!(batches.len(), 2);
        assert_eq!(batches[0].tx_indices, vec![0, 1]);
        assert_eq!(batches[1].tx_indices, vec![2]);
    }

    #[test]
    fn extract_access_set_transfer() {
        let kp = Keypair::generate();
        let to = hash(b"receiver");
        let tx = transfer_tx(&kp, to, 500);
        let access = TransactionScheduler::extract_access_set(&tx);

        // Transfer: payer (writable, signer), receiver (writable), system_program (readonly)
        assert!(
            access.writable.len() >= 2,
            "payer and receiver should be writable"
        );
        assert!(
            !access.readable.is_empty(),
            "system program should be readonly"
        );
    }

    #[test]
    fn batch_indices_preserve_original_order() {
        // Verify that tx_indices always reference the original slice positions
        let kps: Vec<Keypair> = (0..5).map(|_| Keypair::generate()).collect();
        let targets: Vec<Hash> = (0..5)
            .map(|i| hash(format!("target_{i}").as_bytes()))
            .collect();

        let txs: Vec<Transaction> = kps
            .iter()
            .zip(targets.iter())
            .map(|(kp, target)| transfer_tx(kp, *target, 100))
            .collect();

        let batches = TransactionScheduler::schedule(&txs);

        // All indices should appear exactly once across all batches
        let mut all_indices: Vec<usize> = batches
            .iter()
            .flat_map(|b| b.tx_indices.iter().copied())
            .collect();
        all_indices.sort();
        assert_eq!(all_indices, vec![0, 1, 2, 3, 4]);
    }
}
