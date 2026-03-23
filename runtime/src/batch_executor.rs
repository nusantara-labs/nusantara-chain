use nusantara_core::{Account, FeeCalculator, Transaction};
use nusantara_crypto::Hash;
use nusantara_storage::Storage;
use nusantara_vm::ProgramCache;
use tracing::instrument;

use crate::error::RuntimeError;
use crate::slot_commit::SlotCommitter;
use crate::sysvar_cache::SysvarCache;
use crate::transaction_executor::execute_transaction;

pub struct SlotExecutionResult {
    pub slot: u64,
    pub transactions_executed: u64,
    pub transactions_succeeded: u64,
    pub transactions_failed: u64,
    pub total_fees: u64,
    pub total_compute_consumed: u64,
    pub account_delta_hash: Hash,
    /// Aggregated account deltas from all transactions in the slot.
    /// For accounts modified by multiple transactions, the final state is kept.
    /// Sorted by address for deterministic state tree updates.
    pub account_deltas: Vec<(Hash, Account)>,
    /// Transaction statuses collected during execution for inline pubsub.
    /// Each entry is `(tx_hash, status_string)` where status_string is
    /// "success" or "failed: <reason>".
    pub tx_statuses: Vec<(Hash, String)>,
}

#[instrument(skip_all, fields(slot = slot, tx_count = transactions.len()))]
pub fn execute_slot(
    slot: u64,
    transactions: &[Transaction],
    storage: &Storage,
    sysvars: &SysvarCache,
    fee_calculator: &FeeCalculator,
    program_cache: &ProgramCache,
) -> Result<SlotExecutionResult, RuntimeError> {
    let mut committer = SlotCommitter::new();

    for (tx_index, tx) in transactions.iter().enumerate() {
        let result = {
            let cache = committer.account_cache();
            execute_transaction(
                tx,
                storage,
                sysvars,
                fee_calculator,
                slot,
                program_cache,
                Some(cache),
                true,
            )
        };
        committer.commit_result(tx_index, result, slot)?;
        // flush_batch is now a no-op (cache provides cross-batch visibility)
        committer.flush_batch(storage)?;
    }

    // Single RocksDB write at slot end
    committer.flush_all(storage)?;

    let result = committer.finalize(slot);

    metrics::counter!("nusantara_runtime_slot_transactions_total")
        .increment(result.transactions_executed);
    metrics::counter!("nusantara_runtime_slot_fees_collected_total").increment(result.total_fees);
    metrics::counter!("nusantara_runtime_slot_compute_consumed")
        .increment(result.total_compute_consumed);

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nusantara_core::Account;
    use nusantara_core::program::SYSTEM_PROGRAM_ID;
    use nusantara_crypto::{Keypair, hash};

    use crate::test_utils::{test_storage, test_sysvars, transfer_tx};

    #[test]
    fn empty_slot() {
        let (storage, _dir) = test_storage();
        let sysvars = test_sysvars();
        let fee_calc = FeeCalculator::default();

        let cache = ProgramCache::new(16);
        let result = execute_slot(1, &[], &storage, &sysvars, &fee_calc, &cache).unwrap();
        assert_eq!(result.slot, 1);
        assert_eq!(result.transactions_executed, 0);
        assert_eq!(result.transactions_succeeded, 0);
        assert_eq!(result.transactions_failed, 0);
        assert_eq!(result.total_fees, 0);
    }

    #[test]
    fn single_tx() {
        let (storage, _dir) = test_storage();
        let alice_kp = Keypair::generate();
        let alice = alice_kp.address();
        let bob = hash(b"bob");

        storage
            .put_account(&alice, 0, &Account::new(1_000_000, *SYSTEM_PROGRAM_ID))
            .unwrap();

        let tx = transfer_tx(&alice_kp, bob, 100_000);
        let sysvars = test_sysvars();
        let fee_calc = FeeCalculator::default();

        let cache = ProgramCache::new(16);
        let result = execute_slot(1, &[tx], &storage, &sysvars, &fee_calc, &cache).unwrap();
        assert_eq!(result.transactions_executed, 1);
        assert_eq!(result.transactions_succeeded, 1);
        assert_eq!(result.total_fees, 5000);
    }

    #[test]
    fn multiple_tx() {
        let (storage, _dir) = test_storage();
        let alice_kp = Keypair::generate();
        let alice = alice_kp.address();
        let bob = hash(b"bob");
        let carol = hash(b"carol");

        storage
            .put_account(&alice, 0, &Account::new(2_000_000, *SYSTEM_PROGRAM_ID))
            .unwrap();

        let tx1 = transfer_tx(&alice_kp, bob, 100_000);
        let tx2 = transfer_tx(&alice_kp, carol, 50_000);
        let sysvars = test_sysvars();
        let fee_calc = FeeCalculator::default();

        let cache = ProgramCache::new(16);
        let result = execute_slot(1, &[tx1, tx2], &storage, &sysvars, &fee_calc, &cache).unwrap();
        assert_eq!(result.transactions_executed, 2);
        assert_eq!(result.transactions_succeeded, 2);
        assert_eq!(result.total_fees, 10000);
    }

    #[test]
    fn mixed_success_failure() {
        let (storage, _dir) = test_storage();
        let alice_kp = Keypair::generate();
        let alice = alice_kp.address();
        let bob = hash(b"bob");
        let poor_kp = Keypair::generate();
        let poor = poor_kp.address();

        storage
            .put_account(&alice, 0, &Account::new(1_000_000, *SYSTEM_PROGRAM_ID))
            .unwrap();
        storage
            .put_account(&poor, 0, &Account::new(10_000, *SYSTEM_PROGRAM_ID))
            .unwrap();

        let tx1 = transfer_tx(&alice_kp, bob, 100_000); // should succeed
        let tx2 = transfer_tx(&poor_kp, bob, 1_000_000); // should fail (insufficient)
        let sysvars = test_sysvars();
        let fee_calc = FeeCalculator::default();

        let cache = ProgramCache::new(16);
        let result = execute_slot(1, &[tx1, tx2], &storage, &sysvars, &fee_calc, &cache).unwrap();
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
        let result = execute_slot(1, &[tx], &storage, &sysvars, &fee_calc, &cache).unwrap();
        assert_eq!(result.total_fees, 10_000);
    }

    #[test]
    fn delta_hash_deterministic() {
        let (storage1, _dir1) = test_storage();
        let (storage2, _dir2) = test_storage();
        let alice_kp = Keypair::generate();
        let alice = alice_kp.address();
        let bob = hash(b"bob");

        for storage in [&storage1, &storage2] {
            storage
                .put_account(&alice, 0, &Account::new(1_000_000, *SYSTEM_PROGRAM_ID))
                .unwrap();
        }

        let tx = transfer_tx(&alice_kp, bob, 100_000);
        let sysvars = test_sysvars();
        let fee_calc = FeeCalculator::default();

        let cache = ProgramCache::new(16);
        let result1 = execute_slot(
            1,
            std::slice::from_ref(&tx),
            &storage1,
            &sysvars,
            &fee_calc,
            &cache,
        )
        .unwrap();
        let result2 = execute_slot(
            1,
            std::slice::from_ref(&tx),
            &storage2,
            &sysvars,
            &fee_calc,
            &cache,
        )
        .unwrap();

        assert_eq!(result1.account_delta_hash, result2.account_delta_hash);
    }
}
