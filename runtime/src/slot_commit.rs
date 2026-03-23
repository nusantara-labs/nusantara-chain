//! Shared slot-level commit logic used by both sequential and parallel executors.
//!
//! The [`SlotCommitter`] encapsulates the per-transaction delta commit, hash
//! accumulation, status recording, and final result assembly that was previously
//! duplicated between `batch_executor` and `parallel_executor`.

use std::collections::HashMap;

use nusantara_core::Account;
use nusantara_crypto::{Hash, Hasher};
use nusantara_storage::{Storage, StorageWriteBatch, TransactionStatus, TransactionStatusMeta};

use crate::batch_executor::SlotExecutionResult;
use crate::error::RuntimeError;
use crate::transaction_executor::TransactionResult;

/// Accumulates transaction results within a slot and commits them to storage.
///
/// # Determinism invariant
///
/// `commit_result()` must be called in **original transaction index order** so
/// that the delta hasher produces a deterministic hash identical across
/// sequential and parallel execution paths.
///
/// # Batched writes
///
/// Instead of issuing individual RocksDB writes per-account per-transaction,
/// the committer accumulates all writes into a `StorageWriteBatch`. The caller
/// must invoke `flush_batch()` at batch boundaries (e.g., after each parallel
/// scheduling batch) to make writes visible for subsequent batches.
pub(crate) struct SlotCommitter {
    transactions_executed: u64,
    transactions_succeeded: u64,
    transactions_failed: u64,
    total_fees: u64,
    total_compute_consumed: u64,
    delta_hasher: Hasher,
    /// Accumulated RocksDB write batch — flushed once at slot end via `flush_all()`.
    write_batch: StorageWriteBatch,
    /// Collected transaction statuses for inline pubsub (avoids re-reading from storage).
    tx_statuses: Vec<(Hash, String)>,
    /// Unified in-memory cache of committed account states within this slot.
    /// Serves both as cross-batch read cache and as the source for final
    /// account deltas in `finalize()`.
    account_cache: HashMap<Hash, Account>,
}

impl SlotCommitter {
    pub fn new() -> Self {
        Self {
            transactions_executed: 0,
            transactions_succeeded: 0,
            transactions_failed: 0,
            total_fees: 0,
            total_compute_consumed: 0,
            delta_hasher: Hasher::default(),
            write_batch: StorageWriteBatch::new(),
            tx_statuses: Vec::new(),
            account_cache: HashMap::new(),
        }
    }

    /// Commit a single transaction result: accumulate deltas into the write batch.
    ///
    /// Must be called in original transaction index order for deterministic hashing.
    /// Does NOT write to storage — call `flush_batch()` to commit.
    ///
    /// Uses pre-execution account states from `result.loaded_accounts` to skip
    /// redundant `get_account()` RocksDB reads for owner index tracking.
    pub fn commit_result(
        &mut self,
        tx_idx: usize,
        result: TransactionResult,
        slot: u64,
    ) -> Result<(), RuntimeError> {
        self.transactions_executed += 1;
        self.total_fees += result.fee;
        self.total_compute_consumed += result.compute_units_consumed;

        let (status, status_str) = match &result.status {
            Ok(()) => {
                self.transactions_succeeded += 1;
                (TransactionStatus::Success, "success".to_string())
            }
            Err(e) => {
                self.transactions_failed += 1;
                let msg = e.to_string();
                tracing::warn!(
                    slot,
                    tx_idx,
                    error = %e,
                    fee = result.fee,
                    "transaction failed"
                );
                (
                    TransactionStatus::Failed(msg.clone()),
                    format!("failed: {msg}"),
                )
            }
        };

        // Collect status for inline pubsub
        self.tx_statuses.push((result.tx_hash, status_str));

        // Accumulate account deltas directly into write batch (no intermediate allocations).
        // Uses loaded_accounts for owner index tracking instead of re-reading from RocksDB.
        for (address, account) in &result.account_deltas {
            let old_account = result.loaded_accounts.get(address);

            Storage::append_account_write_with_old(
                &mut self.write_batch,
                address,
                slot,
                account,
                old_account,
            )
            .map_err(|e| RuntimeError::InvalidAccountData(e.to_string()))?;

            // Feed into delta hash — streaming fields matches borsh layout exactly:
            // lamports(u64 LE) + data(u32 len LE + bytes) + owner(64 raw) + executable(u8) + rent_epoch(u64 LE)
            self.delta_hasher.update(address.as_bytes());
            self.delta_hasher.update(&account.lamports.to_le_bytes());
            self.delta_hasher.update(&(account.data.len() as u32).to_le_bytes());
            self.delta_hasher.update(&account.data);
            self.delta_hasher.update(account.owner.as_bytes());
            self.delta_hasher.update(&[account.executable as u8]);
            self.delta_hasher.update(&account.rent_epoch.to_le_bytes());

            // Track final state for each address (last write wins) and
            // update in-memory cache for cross-batch reads.
            self.account_cache.insert(*address, account.clone());
        }

        // Accumulate transaction status directly into write batch
        let meta = TransactionStatusMeta {
            slot,
            status,
            fee: result.fee,
            pre_balances: result.pre_balances,
            post_balances: result.post_balances,
            compute_units_consumed: result.compute_units_consumed,
        };
        Storage::append_transaction_status(&mut self.write_batch, &result.tx_hash, &meta)?;

        // Accumulate address signatures directly into write batch
        for (address, _) in &result.account_deltas {
            Storage::append_address_signature(
                &mut self.write_batch,
                address,
                slot,
                tx_idx as u32,
                &result.tx_hash,
            );
        }

        Ok(())
    }

    /// Get read-only reference to the account cache for cross-batch reads.
    pub fn account_cache(&self) -> &HashMap<Hash, Account> {
        &self.account_cache
    }

    /// No-op: writes are deferred to `flush_all()` at slot end.
    /// The `account_cache` provides cross-batch visibility.
    pub fn flush_batch(&mut self, _storage: &Storage) -> Result<(), RuntimeError> {
        Ok(())
    }

    /// Flush all accumulated writes to RocksDB in a single atomic batch.
    /// Called once at the end of slot execution.
    pub fn flush_all(&self, storage: &Storage) -> Result<(), RuntimeError> {
        if !self.write_batch.is_empty() {
            storage.write(&self.write_batch)?;
        }
        Ok(())
    }

    /// Extract the accumulated write batch WITHOUT flushing to storage.
    ///
    /// Used by `execute_slot_parallel_deferred` so the caller can verify
    /// execution results before committing to storage.
    pub fn take_write_batch(self) -> (StorageWriteBatch, SlotCommitter) {
        let batch = self.write_batch;
        let rest = SlotCommitter {
            transactions_executed: self.transactions_executed,
            transactions_succeeded: self.transactions_succeeded,
            transactions_failed: self.transactions_failed,
            total_fees: self.total_fees,
            total_compute_consumed: self.total_compute_consumed,
            delta_hasher: self.delta_hasher,
            write_batch: StorageWriteBatch::new(),
            tx_statuses: self.tx_statuses,
            account_cache: self.account_cache,
        };
        (batch, rest)
    }

    /// Finalize the slot: compute delta hash, sort deltas, and return the result.
    pub fn finalize(self, slot: u64) -> SlotExecutionResult {
        let account_delta_hash = self.delta_hasher.finalize();

        // Sort deltas by address for deterministic state tree updates
        let mut account_deltas: Vec<(Hash, Account)> = self.account_cache.into_iter().collect();
        account_deltas.sort_by_key(|(addr, _)| *addr);

        SlotExecutionResult {
            slot,
            transactions_executed: self.transactions_executed,
            transactions_succeeded: self.transactions_succeeded,
            transactions_failed: self.transactions_failed,
            total_fees: self.total_fees,
            total_compute_consumed: self.total_compute_consumed,
            account_delta_hash,
            account_deltas,
            tx_statuses: self.tx_statuses,
        }
    }
}
