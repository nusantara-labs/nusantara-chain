//! Parallel slot execution using Sealevel-style transaction scheduling.
//!
//! Transactions are grouped into non-conflicting batches by the
//! [`TransactionScheduler`], and each batch is executed in parallel using
//! [`rayon`]. The key invariant is that `execute_slot_parallel()` produces an
//! **identical** `account_delta_hash` as the sequential [`execute_slot()`] for
//! the same input, because deltas are committed and hashed in **original
//! transaction order** regardless of which thread executed them.
//!
//! # Concurrency model
//!
//! Within each batch, transactions touch disjoint account sets (guaranteed by
//! the scheduler). Each transaction independently loads its accounts from
//! storage, executes, and produces a `TransactionResult`. After the batch
//! completes, results are sorted by original transaction index and committed
//! to storage sequentially. This sequential commit phase ensures determinism.
//!
//! # Deadlock prevention
//!
//! No locks are held across the rayon parallel scope. Storage reads inside the
//! parallel section use point-get operations on RocksDB which are inherently
//! lock-free from the caller's perspective. The `ProgramCache` uses a
//! `parking_lot::Mutex` with very short critical sections (no `.await`).

use nusantara_core::{FeeCalculator, Transaction};
use nusantara_storage::{Storage, StorageWriteBatch};
use nusantara_vm::ProgramCache;
use rayon::prelude::*;
use tracing::instrument;

use crate::batch_executor::SlotExecutionResult;
use crate::error::RuntimeError;
use crate::scheduler::TransactionScheduler;
use crate::slot_commit::SlotCommitter;
use crate::sysvar_cache::SysvarCache;
use crate::transaction_executor::{TransactionResult, execute_transaction};

/// Execute a slot's transactions in parallel batches.
///
/// # Determinism guarantee
///
/// The `account_delta_hash` is computed by feeding account deltas in the
/// **original transaction order** (not execution order). Within each batch,
/// rayon may execute transactions in any order, but results are collected and
/// sorted by their original index before being committed and hashed.
///
/// This means the output is byte-identical to [`crate::batch_executor::execute_slot()`]
/// for the same input.
///
/// # Algorithm
///
/// 1. Schedule transactions into non-conflicting parallel batches.
/// 2. For each batch:
///    a. Execute all transactions in parallel via rayon.
///    b. Sort results by original transaction index.
///    c. Commit deltas to storage in original order.
///    d. Feed delta hasher in original order.
/// 3. Return aggregated results.
#[instrument(skip_all, fields(slot = slot, tx_count = transactions.len()))]
pub fn execute_slot_parallel(
    slot: u64,
    transactions: &[Transaction],
    storage: &Storage,
    sysvars: &SysvarCache,
    fee_calculator: &FeeCalculator,
    program_cache: &ProgramCache,
) -> Result<SlotExecutionResult, RuntimeError> {
    let mut committer = SlotCommitter::new();

    // Step 1: Schedule transactions into parallel batches
    let batches = TransactionScheduler::schedule(transactions);

    // Step 2: Execute each batch
    for batch in &batches {
        // Execute transactions within this batch in parallel.
        // Safety: the scheduler guarantees no two transactions in the same
        // batch touch the same writable account, so parallel execution with
        // independent snapshots is safe.
        //
        // Block scope ensures immutable borrow of committer (through cache)
        // ends before the mutable borrow in commit_result().
        let results: Vec<(usize, TransactionResult)> = {
            let cache = committer.account_cache();
            batch
                .tx_indices
                .par_iter()
                .map(|&tx_idx| {
                    let tx = &transactions[tx_idx];
                    let result = execute_transaction(
                        tx,
                        storage,
                        sysvars,
                        fee_calculator,
                        slot,
                        program_cache,
                        Some(cache),
                        true,
                    );
                    (tx_idx, result)
                })
                .collect()
        };

        // Sort by original transaction index for deterministic commit order
        let mut sorted_results = results;
        sorted_results.sort_by_key(|(idx, _)| *idx);

        // Commit results in original order (deterministic delta hash)
        for (tx_idx, result) in sorted_results {
            committer.commit_result(tx_idx, result, slot)?;
        }

        // flush_batch is now a no-op (cache provides cross-batch visibility)
        committer.flush_batch(storage)?;
    }

    // Single RocksDB write at slot end
    committer.flush_all(storage)?;

    let result = committer.finalize(slot);

    metrics::counter!("nusantara_runtime_parallel_slot_transactions_total")
        .increment(result.transactions_executed);
    metrics::counter!("nusantara_runtime_parallel_slot_fees_collected_total")
        .increment(result.total_fees);
    metrics::counter!("nusantara_runtime_parallel_slot_compute_consumed")
        .increment(result.total_compute_consumed);
    metrics::counter!("nusantara_runtime_parallel_batches_total").increment(batches.len() as u64);

    Ok(result)
}

/// Result of deferred slot execution: the execution result + the uncommitted
/// write batch. The caller must commit the batch to storage after verification.
pub struct DeferredSlotExecution {
    pub result: SlotExecutionResult,
    pub write_batch: StorageWriteBatch,
}

/// Execute a slot's transactions in parallel batches WITHOUT writing to storage.
///
/// Identical to [`execute_slot_parallel`] but the account deltas, transaction
/// statuses, and address signatures are accumulated in a `StorageWriteBatch`
/// that is returned to the caller instead of being flushed. This allows the
/// caller to verify the execution results (e.g., bank_hash) BEFORE committing
/// the writes, preventing storage pollution on verification failure.
///
/// # Usage
///
/// ```ignore
/// let deferred = execute_slot_parallel_deferred(slot, txs, storage, ...)?;
/// // verify bank_hash, merkle_root, etc.
/// if verification_ok {
///     storage.write(&deferred.write_batch)?;
/// }
/// ```
#[instrument(skip_all, fields(slot = slot, tx_count = transactions.len()))]
pub fn execute_slot_parallel_deferred(
    slot: u64,
    transactions: &[Transaction],
    storage: &Storage,
    sysvars: &SysvarCache,
    fee_calculator: &FeeCalculator,
    program_cache: &ProgramCache,
) -> Result<DeferredSlotExecution, RuntimeError> {
    let mut committer = SlotCommitter::new();

    let batches = TransactionScheduler::schedule(transactions);

    for batch in &batches {
        let results: Vec<(usize, TransactionResult)> = {
            let cache = committer.account_cache();
            batch
                .tx_indices
                .par_iter()
                .map(|&tx_idx| {
                    let tx = &transactions[tx_idx];
                    let result = execute_transaction(
                        tx,
                        storage,
                        sysvars,
                        fee_calculator,
                        slot,
                        program_cache,
                        Some(cache),
                        true,
                    );
                    (tx_idx, result)
                })
                .collect()
        };

        let mut sorted_results = results;
        sorted_results.sort_by_key(|(idx, _)| *idx);

        for (tx_idx, result) in sorted_results {
            committer.commit_result(tx_idx, result, slot)?;
        }

        committer.flush_batch(storage)?;
    }

    // Extract the write batch WITHOUT flushing to storage
    let (write_batch, committer) = committer.take_write_batch();
    let result = committer.finalize(slot);

    metrics::counter!("nusantara_runtime_parallel_slot_transactions_total")
        .increment(result.transactions_executed);
    metrics::counter!("nusantara_runtime_parallel_slot_fees_collected_total").increment(result.total_fees);
    metrics::counter!("nusantara_runtime_parallel_slot_compute_consumed")
        .increment(result.total_compute_consumed);
    metrics::counter!("nusantara_runtime_parallel_batches_total").increment(batches.len() as u64);

    Ok(DeferredSlotExecution {
        result,
        write_batch,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use nusantara_core::Account;
    use nusantara_core::program::SYSTEM_PROGRAM_ID;
    use nusantara_crypto::{Hash, Keypair, hash};

    use crate::test_utils::{test_storage, test_sysvars, transfer_tx};

    // ---------------------------------------------------------------
    // Determinism: parallel must produce identical delta hash as sequential
    // ---------------------------------------------------------------

    #[test]
    fn determinism_single_tx() {
        let alice_kp = Keypair::generate();
        let alice = alice_kp.address();
        let bob = hash(b"bob");

        let tx = transfer_tx(&alice_kp, bob, 100_000);
        let sysvars = test_sysvars();
        let fee_calc = FeeCalculator::default();
        let cache = ProgramCache::new(16);

        // Sequential
        let (storage_seq, _d1) = test_storage();
        storage_seq
            .put_account(&alice, 0, &Account::new(1_000_000, *SYSTEM_PROGRAM_ID))
            .unwrap();
        let seq_result = crate::batch_executor::execute_slot(
            1,
            std::slice::from_ref(&tx),
            &storage_seq,
            &sysvars,
            &fee_calc,
            &cache,
        )
        .unwrap();

        // Parallel
        let (storage_par, _d2) = test_storage();
        storage_par
            .put_account(&alice, 0, &Account::new(1_000_000, *SYSTEM_PROGRAM_ID))
            .unwrap();
        let par_result = execute_slot_parallel(
            1,
            std::slice::from_ref(&tx),
            &storage_par,
            &sysvars,
            &fee_calc,
            &cache,
        )
        .unwrap();

        assert_eq!(seq_result.account_delta_hash, par_result.account_delta_hash);
        assert_eq!(
            seq_result.transactions_executed,
            par_result.transactions_executed
        );
        assert_eq!(
            seq_result.transactions_succeeded,
            par_result.transactions_succeeded
        );
        assert_eq!(seq_result.total_fees, par_result.total_fees);
    }

    #[test]
    fn determinism_multiple_conflicting_txs() {
        // Same payer sends two transfers -> must be sequential batches
        let alice_kp = Keypair::generate();
        let alice = alice_kp.address();
        let bob = hash(b"bob");
        let carol = hash(b"carol");

        let tx1 = transfer_tx(&alice_kp, bob, 100_000);
        let tx2 = transfer_tx(&alice_kp, carol, 50_000);
        let txs = [tx1, tx2];
        let sysvars = test_sysvars();
        let fee_calc = FeeCalculator::default();
        let cache = ProgramCache::new(16);

        // Sequential
        let (storage_seq, _d1) = test_storage();
        storage_seq
            .put_account(&alice, 0, &Account::new(2_000_000, *SYSTEM_PROGRAM_ID))
            .unwrap();
        let seq_result =
            crate::batch_executor::execute_slot(1, &txs, &storage_seq, &sysvars, &fee_calc, &cache)
                .unwrap();

        // Parallel
        let (storage_par, _d2) = test_storage();
        storage_par
            .put_account(&alice, 0, &Account::new(2_000_000, *SYSTEM_PROGRAM_ID))
            .unwrap();
        let par_result =
            execute_slot_parallel(1, &txs, &storage_par, &sysvars, &fee_calc, &cache).unwrap();

        assert_eq!(seq_result.account_delta_hash, par_result.account_delta_hash);
        assert_eq!(
            seq_result.transactions_executed,
            par_result.transactions_executed
        );
        assert_eq!(
            seq_result.transactions_succeeded,
            par_result.transactions_succeeded
        );
        assert_eq!(
            seq_result.transactions_failed,
            par_result.transactions_failed
        );
        assert_eq!(seq_result.total_fees, par_result.total_fees);
    }

    #[test]
    fn determinism_independent_transfers() {
        // Independent payers -> can run in parallel batch
        let alice_kp = Keypair::generate();
        let carol_kp = Keypair::generate();
        let alice = alice_kp.address();
        let carol = carol_kp.address();
        let bob = hash(b"bob");
        let dave = hash(b"dave");

        let tx1 = transfer_tx(&alice_kp, bob, 100_000);
        let tx2 = transfer_tx(&carol_kp, dave, 200_000);
        let txs = [tx1, tx2];
        let sysvars = test_sysvars();
        let fee_calc = FeeCalculator::default();
        let cache = ProgramCache::new(16);

        // Sequential
        let (storage_seq, _d1) = test_storage();
        storage_seq
            .put_account(&alice, 0, &Account::new(1_000_000, *SYSTEM_PROGRAM_ID))
            .unwrap();
        storage_seq
            .put_account(&carol, 0, &Account::new(1_000_000, *SYSTEM_PROGRAM_ID))
            .unwrap();
        let seq_result =
            crate::batch_executor::execute_slot(1, &txs, &storage_seq, &sysvars, &fee_calc, &cache)
                .unwrap();

        // Parallel
        let (storage_par, _d2) = test_storage();
        storage_par
            .put_account(&alice, 0, &Account::new(1_000_000, *SYSTEM_PROGRAM_ID))
            .unwrap();
        storage_par
            .put_account(&carol, 0, &Account::new(1_000_000, *SYSTEM_PROGRAM_ID))
            .unwrap();
        let par_result =
            execute_slot_parallel(1, &txs, &storage_par, &sysvars, &fee_calc, &cache).unwrap();

        assert_eq!(seq_result.account_delta_hash, par_result.account_delta_hash);
        assert_eq!(
            seq_result.transactions_executed,
            par_result.transactions_executed
        );
        assert_eq!(
            seq_result.transactions_succeeded,
            par_result.transactions_succeeded
        );
        assert_eq!(seq_result.total_fees, par_result.total_fees);
    }

    // ---------------------------------------------------------------
    // Edge cases
    // ---------------------------------------------------------------

    #[test]
    fn empty_slot() {
        let (storage, _dir) = test_storage();
        let sysvars = test_sysvars();
        let fee_calc = FeeCalculator::default();
        let cache = ProgramCache::new(16);

        let result = execute_slot_parallel(1, &[], &storage, &sysvars, &fee_calc, &cache).unwrap();
        assert_eq!(result.slot, 1);
        assert_eq!(result.transactions_executed, 0);
        assert_eq!(result.transactions_succeeded, 0);
        assert_eq!(result.transactions_failed, 0);
        assert_eq!(result.total_fees, 0);
    }

    #[test]
    fn mixed_success_failure() {
        let (storage, _dir) = test_storage();
        let alice_kp = Keypair::generate();
        let alice = alice_kp.address();
        let poor_kp = Keypair::generate();
        let poor = poor_kp.address();
        let bob = hash(b"bob");

        storage
            .put_account(&alice, 0, &Account::new(1_000_000, *SYSTEM_PROGRAM_ID))
            .unwrap();
        storage
            .put_account(&poor, 0, &Account::new(10_000, *SYSTEM_PROGRAM_ID))
            .unwrap();

        let tx1 = transfer_tx(&alice_kp, bob, 100_000); // success
        let tx2 = transfer_tx(&poor_kp, bob, 1_000_000); // fail: insufficient

        // These conflict on bob (both write) so they will be in separate batches
        let txs = [tx1, tx2];
        let sysvars = test_sysvars();
        let fee_calc = FeeCalculator::default();
        let cache = ProgramCache::new(16);

        let result = execute_slot_parallel(1, &txs, &storage, &sysvars, &fee_calc, &cache).unwrap();
        assert_eq!(result.transactions_executed, 2);
        assert_eq!(result.transactions_succeeded, 1);
        assert_eq!(result.transactions_failed, 1);
    }

    #[test]
    fn fee_collection() {
        let (storage, _dir) = test_storage();
        let alice_kp = Keypair::generate();
        let alice = alice_kp.address();
        let bob = hash(b"bob");

        storage
            .put_account(&alice, 0, &Account::new(1_000_000, *SYSTEM_PROGRAM_ID))
            .unwrap();

        let tx = transfer_tx(&alice_kp, bob, 100);
        let sysvars = test_sysvars();
        let fee_calc = FeeCalculator::new(10_000);
        let cache = ProgramCache::new(16);

        let result = execute_slot_parallel(
            1,
            std::slice::from_ref(&tx),
            &storage,
            &sysvars,
            &fee_calc,
            &cache,
        )
        .unwrap();
        assert_eq!(result.total_fees, 10_000);
    }

    #[test]
    fn determinism_repeated_runs() {
        // Run the same parallel execution 5 times and verify identical hashes
        let alice_kp = Keypair::generate();
        let carol_kp = Keypair::generate();
        let alice = alice_kp.address();
        let carol = carol_kp.address();
        let bob = hash(b"bob");
        let dave = hash(b"dave");

        let tx1 = transfer_tx(&alice_kp, bob, 50_000);
        let tx2 = transfer_tx(&carol_kp, dave, 75_000);
        let txs = [tx1, tx2];
        let sysvars = test_sysvars();
        let fee_calc = FeeCalculator::default();
        let cache = ProgramCache::new(16);

        let mut hashes = Vec::new();
        for _ in 0..5 {
            let (storage, _dir) = test_storage();
            storage
                .put_account(&alice, 0, &Account::new(1_000_000, *SYSTEM_PROGRAM_ID))
                .unwrap();
            storage
                .put_account(&carol, 0, &Account::new(1_000_000, *SYSTEM_PROGRAM_ID))
                .unwrap();

            let result =
                execute_slot_parallel(1, &txs, &storage, &sysvars, &fee_calc, &cache).unwrap();
            hashes.push(result.account_delta_hash);
        }

        // All hashes must be identical
        for h in &hashes[1..] {
            assert_eq!(hashes[0], *h, "parallel execution must be deterministic");
        }
    }

    #[test]
    fn determinism_many_independent_txs() {
        // 10 independent transfers from different payers
        let keypairs: Vec<Keypair> = (0..10).map(|_| Keypair::generate()).collect();
        let targets: Vec<Hash> = (0..10)
            .map(|i| hash(format!("target_{i}").as_bytes()))
            .collect();

        let txs: Vec<Transaction> = keypairs
            .iter()
            .zip(targets.iter())
            .map(|(kp, target)| transfer_tx(kp, *target, 50_000))
            .collect();

        let sysvars = test_sysvars();
        let fee_calc = FeeCalculator::default();
        let cache = ProgramCache::new(16);

        // Sequential baseline
        let (storage_seq, _d1) = test_storage();
        for kp in &keypairs {
            storage_seq
                .put_account(
                    &kp.address(),
                    0,
                    &Account::new(1_000_000, *SYSTEM_PROGRAM_ID),
                )
                .unwrap();
        }
        let seq_result =
            crate::batch_executor::execute_slot(1, &txs, &storage_seq, &sysvars, &fee_calc, &cache)
                .unwrap();

        // Parallel
        let (storage_par, _d2) = test_storage();
        for kp in &keypairs {
            storage_par
                .put_account(
                    &kp.address(),
                    0,
                    &Account::new(1_000_000, *SYSTEM_PROGRAM_ID),
                )
                .unwrap();
        }
        let par_result =
            execute_slot_parallel(1, &txs, &storage_par, &sysvars, &fee_calc, &cache).unwrap();

        assert_eq!(seq_result.account_delta_hash, par_result.account_delta_hash);
        assert_eq!(
            seq_result.transactions_executed,
            par_result.transactions_executed
        );
        assert_eq!(seq_result.total_fees, par_result.total_fees);
    }
}
