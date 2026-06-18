use std::collections::HashMap;

use nusantara_core::{Account, FeeCalculator, Message, Transaction};
use nusantara_crypto::Hash;
use nusantara_storage::Storage;
use nusantara_vm::ProgramCache;
use tracing::instrument;

use crate::account_loader::load_accounts;
use crate::compute_budget_parser::parse_compute_budget;
use crate::error::RuntimeError;
use crate::program_dispatch::{SIGNATURE_VERIFY_COST, dispatch_instruction};
use crate::sysvar_cache::SysvarCache;
use crate::transaction_context::TransactionContext;

/// Validate that all instruction account indices are within bounds.
///
/// This runs before `parse_compute_budget` and account loading so a
/// wire-deserialized transaction with a crafted `program_id_index` or accounts
/// list cannot panic the executor with an out-of-bounds index access.
pub(crate) fn sanitize_message(message: &Message) -> Result<(), RuntimeError> {
    let n = message.account_keys.len();
    for (ix_idx, ix) in message.instructions.iter().enumerate() {
        let pid = ix.program_id_index as usize;
        if pid >= n {
            return Err(RuntimeError::InvalidInstructionData(format!(
                "instruction {ix_idx}: program_id_index {pid} out of bounds \
                 (account_keys len {n})"
            )));
        }
        for (ai, &acc_idx) in ix.accounts.iter().enumerate() {
            let acc = acc_idx as usize;
            if acc >= n {
                return Err(RuntimeError::InvalidInstructionData(format!(
                    "instruction {ix_idx}: account index {ai} value {acc} out of bounds \
                     (account_keys len {n})"
                )));
            }
        }
    }
    Ok(())
}

pub struct TransactionResult {
    pub tx_hash: Hash,
    pub status: Result<(), RuntimeError>,
    pub fee: u64,
    pub compute_units_consumed: u64,
    pub account_deltas: Vec<(Hash, Account)>,
    pub pre_balances: Vec<u64>,
    pub post_balances: Vec<u64>,
    /// Pre-execution account states (as loaded from storage).
    /// Used during commit to skip redundant `get_account()` RocksDB reads
    /// for owner index tracking. Keyed by address for O(1) lookups.
    pub loaded_accounts: HashMap<Hash, Account>,
}

#[instrument(skip_all, fields(slot = slot))]
#[allow(clippy::too_many_arguments)]
pub fn execute_transaction(
    tx: &Transaction,
    storage: &Storage,
    sysvars: &SysvarCache,
    fee_calculator: &FeeCalculator,
    slot: u64,
    program_cache: &ProgramCache,
    account_cache: Option<&HashMap<Hash, Account>>,
    skip_sig_verify: bool,
) -> TransactionResult {
    // Step 1: Compute hash
    let tx_hash = tx.hash();

    // Step 1.2: Sanitize message — verify all instruction indices are in bounds.
    // This must run before parse_compute_budget (which indexes account_keys) to
    // prevent a panic from a crafted program_id_index on wire-deserialized txs.
    if let Err(e) = sanitize_message(&tx.message) {
        return TransactionResult {
            tx_hash,
            status: Err(e),
            fee: 0,
            compute_units_consumed: 0,
            account_deltas: vec![],
            pre_balances: vec![],
            post_balances: vec![],
            loaded_accounts: HashMap::new(),
        };
    }

    // Step 1.5: Verify signatures (skip when already verified at TPU ingress)
    if !skip_sig_verify
        && let Err(e) = tx.verify_signatures()
    {
        return TransactionResult {
            tx_hash,
            status: Err(RuntimeError::SignatureVerificationFailed(e.to_string())),
            fee: 0,
            compute_units_consumed: 0,
            account_deltas: vec![],
            pre_balances: vec![],
            post_balances: vec![],
            loaded_accounts: HashMap::new(),
        };
    }

    // Step 2: Parse compute budget
    let compute_budget = match parse_compute_budget(&tx.message) {
        Ok(budget) => budget,
        Err(e) => {
            return TransactionResult {
                tx_hash,
                status: Err(e),
                fee: 0,
                compute_units_consumed: 0,
                account_deltas: vec![],
                pre_balances: vec![],
                post_balances: vec![],
                loaded_accounts: HashMap::new(),
            };
        }
    };

    // Step 3: Calculate fee
    let fee = fee_calculator.calculate_fee(tx.message.num_required_signatures as u64);

    // Step 4: Load accounts (cache-first, fallback to RocksDB)
    let loaded = match load_accounts(
        storage,
        &tx.message.account_keys,
        compute_budget.loaded_accounts_data_size_limit,
        account_cache,
    ) {
        Ok(l) => l,
        Err(e) => {
            return TransactionResult {
                tx_hash,
                status: Err(e),
                fee: 0,
                compute_units_consumed: 0,
                account_deltas: vec![],
                pre_balances: vec![],
                post_balances: vec![],
                loaded_accounts: HashMap::new(),
            };
        }
    };

    // Capture pre-execution account states for owner index tracking during commit.
    // This eliminates redundant get_account() RocksDB reads in the commit phase.
    let loaded_accounts: HashMap<Hash, Account> = loaded
        .accounts
        .iter()
        .map(|(addr, acc)| (*addr, acc.clone()))
        .collect();

    let pre_balances: Vec<u64> = loaded.accounts.iter().map(|(_, a)| a.lamports).collect();

    // Step 5: Deduct fee from payer (account_keys[0]) — permanent even on failure
    let mut fee_accounts = loaded.accounts;
    if fee_accounts[0].1.lamports < fee {
        return TransactionResult {
            tx_hash,
            status: Err(RuntimeError::InsufficientFunds {
                needed: fee,
                available: fee_accounts[0].1.lamports,
            }),
            fee: 0,
            compute_units_consumed: 0,
            account_deltas: vec![],
            pre_balances: pre_balances.clone(),
            post_balances: pre_balances,
            loaded_accounts,
        };
    }
    fee_accounts[0].1.lamports -= fee;

    // Step 6: Verify recent blockhash
    // Allow Hash::zero() only at genesis (slot 0)
    if !(sysvars.contains_blockhash(&tx.message.recent_blockhash)
        || slot == 0 && tx.message.recent_blockhash == Hash::zero())
    {
        // Fee is still collected, return fee-only delta
        let post_balances: Vec<u64> = fee_accounts.iter().map(|(_, a)| a.lamports).collect();
        let payer_delta = vec![(fee_accounts[0].0, fee_accounts[0].1.clone())];
        return TransactionResult {
            tx_hash,
            status: Err(RuntimeError::BlockhashNotFound),
            fee,
            compute_units_consumed: 0,
            account_deltas: payer_delta,
            pre_balances,
            post_balances,
            loaded_accounts,
        };
    }

    // Step 7: Create TransactionContext
    let mut ctx = TransactionContext::new(
        fee_accounts,
        tx.message.clone(),
        slot,
        compute_budget.compute_unit_limit,
    );

    // Step 8: Charge signature cost
    let sig_cost = tx.message.num_required_signatures as u64 * SIGNATURE_VERIFY_COST;
    if let Err(e) = ctx.consume_compute(sig_cost) {
        let post_balances = ctx.post_balances();
        let payer = ctx.get_account(0).unwrap();
        let payer_delta = vec![(*payer.address, payer.account.clone())];
        return TransactionResult {
            tx_hash,
            status: Err(e),
            fee,
            compute_units_consumed: ctx.compute_consumed(),
            account_deltas: payer_delta,
            pre_balances,
            post_balances,
            loaded_accounts,
        };
    }

    // Step 9: Execute instructions
    let exec_result = execute_instructions(&mut ctx, sysvars, program_cache);

    // Step 10: Post-verify and collect results
    let compute_units_consumed = ctx.compute_consumed();

    match exec_result {
        Ok(()) => {
            // Verify invariants
            if let Err(e) = ctx.verify_invariants() {
                let post_balances = ctx.post_balances();
                let payer = ctx.get_account(0).unwrap();
                let payer_delta = vec![(*payer.address, payer.account.clone())];
                metrics::counter!("nusantara_runtime_transactions_failed_total").increment(1);
                return TransactionResult {
                    tx_hash,
                    status: Err(e),
                    fee,
                    compute_units_consumed,
                    account_deltas: payer_delta,
                    pre_balances,
                    post_balances,
                    loaded_accounts,
                };
            }

            let post_balances = ctx.post_balances();
            let deltas = ctx.collect_account_deltas();
            metrics::counter!("nusantara_runtime_transactions_executed_total").increment(1);
            metrics::counter!("nusantara_runtime_compute_units_consumed")
                .increment(compute_units_consumed);
            TransactionResult {
                tx_hash,
                status: Ok(()),
                fee,
                compute_units_consumed,
                account_deltas: deltas,
                pre_balances,
                post_balances,
                loaded_accounts,
            }
        }
        Err(e) => {
            let post_balances = ctx.post_balances();
            let payer = ctx.get_account(0).unwrap();
            let payer_delta = vec![(*payer.address, payer.account.clone())];
            metrics::counter!("nusantara_runtime_transactions_failed_total").increment(1);
            TransactionResult {
                tx_hash,
                status: Err(e),
                fee,
                compute_units_consumed,
                account_deltas: payer_delta,
                pre_balances,
                post_balances,
                loaded_accounts,
            }
        }
    }
}

fn execute_instructions(
    ctx: &mut TransactionContext,
    sysvars: &SysvarCache,
    program_cache: &ProgramCache,
) -> Result<(), RuntimeError> {
    for ix_idx in 0..ctx.message().instructions.len() {
        let program_id_index = ctx.message().instructions[ix_idx].program_id_index as usize;
        // Belt-and-suspenders: sanitize_message already validated these indices
        // before we reached this point; these .get() calls guard against any
        // future code path that bypasses sanitize_message.
        let program_id = *ctx
            .message()
            .account_keys
            .get(program_id_index)
            .ok_or_else(|| {
                RuntimeError::InvalidInstructionData(format!(
                    "instruction {ix_idx}: program_id_index {program_id_index} out of bounds"
                ))
            })?;
        let accounts = ctx.message().instructions[ix_idx].accounts.clone();
        let data = ctx.message().instructions[ix_idx].data.clone();
        dispatch_instruction(&program_id, &accounts, &data, ctx, sysvars, program_cache)?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use nusantara_core::program::SYSTEM_PROGRAM_ID;
    use nusantara_core::{EpochSchedule, Message};
    use nusantara_crypto::{Keypair, hash};
    use nusantara_rent_program::Rent;
    use nusantara_sysvar_program::{Clock, RecentBlockhashes, SlotHashes, StakeHistory};

    use crate::test_utils::{test_storage, test_sysvars};

    fn create_signed_transfer_tx(kp: &Keypair, to: Hash, amount: u64) -> Transaction {
        let from = kp.address();
        let ix = nusantara_system_program::transfer(&from, &to, amount);
        let msg = Message::new(&[ix], &from).unwrap();
        let mut tx = Transaction::new(msg);
        tx.sign(&[kp]);
        tx
    }

    #[test]
    fn simple_transfer() {
        let (storage, _dir) = test_storage();
        let kp = Keypair::generate();
        let from = kp.address();
        let to = hash(b"bob");
        let fee_calc = FeeCalculator::default();

        storage
            .put_account(&from, 0, &Account::new(1_000_000, *SYSTEM_PROGRAM_ID))
            .unwrap();
        storage
            .put_account(&to, 0, &Account::new(500_000, *SYSTEM_PROGRAM_ID))
            .unwrap();

        let tx = create_signed_transfer_tx(&kp, to, 100_000);
        let sysvars = test_sysvars();
        let cache = ProgramCache::new(16);
        let result = execute_transaction(&tx, &storage, &sysvars, &fee_calc, 1, &cache, None, false);

        assert!(result.status.is_ok());
        assert_eq!(result.fee, 5000);
        assert!(result.compute_units_consumed > 0);
        assert!(!result.account_deltas.is_empty());
    }

    #[test]
    fn create_account_tx() {
        let (storage, _dir) = test_storage();
        let kp = Keypair::generate();
        let kp_new = Keypair::generate();
        let from = kp.address();
        let new_acc = kp_new.address();
        let owner = hash(b"owner_program");
        let rent = Rent::default();
        let min = rent.minimum_balance(100);

        storage
            .put_account(&from, 0, &Account::new(min + 1_000_000, *SYSTEM_PROGRAM_ID))
            .unwrap();

        let ix = nusantara_system_program::create_account(&from, &new_acc, min, 100, &owner);
        let msg = Message::new(&[ix], &from).unwrap();
        let mut tx = Transaction::new(msg);
        tx.sign(&[&kp, &kp_new]);
        let sysvars = test_sysvars();
        let fee_calc = FeeCalculator::default();
        let cache = ProgramCache::new(16);
        let result = execute_transaction(&tx, &storage, &sysvars, &fee_calc, 1, &cache, None, false);

        assert!(result.status.is_ok());
    }

    #[test]
    fn insufficient_fee() {
        let (storage, _dir) = test_storage();
        let kp = Keypair::generate();
        let from = kp.address();
        let to = hash(b"bob");

        storage
            .put_account(&from, 0, &Account::new(100, *SYSTEM_PROGRAM_ID))
            .unwrap();

        let tx = create_signed_transfer_tx(&kp, to, 50);
        let sysvars = test_sysvars();
        let fee_calc = FeeCalculator::new(200);
        let cache = ProgramCache::new(16);
        let result = execute_transaction(&tx, &storage, &sysvars, &fee_calc, 1, &cache, None, false);

        assert!(result.status.is_err());
        assert!(matches!(
            result.status.unwrap_err(),
            RuntimeError::InsufficientFunds { .. }
        ));
    }

    #[test]
    fn blockhash_not_found() {
        let (storage, _dir) = test_storage();
        let kp = Keypair::generate();
        let from = kp.address();
        let to = hash(b"bob");

        storage
            .put_account(&from, 0, &Account::new(1_000_000, *SYSTEM_PROGRAM_ID))
            .unwrap();

        let ix = nusantara_system_program::transfer(&from, &to, 100);
        let mut msg = Message::new(&[ix], &from).unwrap();
        msg.recent_blockhash = hash(b"stale_blockhash");

        let mut tx = Transaction::new(msg);
        tx.sign(&[&kp]);
        let sysvars = SysvarCache::new(
            Clock::default(),
            Rent::default(),
            EpochSchedule::default(),
            SlotHashes::default(),
            StakeHistory::default(),
            RecentBlockhashes::new(vec![hash(b"valid_blockhash")]),
        );
        let fee_calc = FeeCalculator::default();
        let cache = ProgramCache::new(16);
        let result = execute_transaction(&tx, &storage, &sysvars, &fee_calc, 1, &cache, None, false);

        assert!(result.status.is_err());
        assert!(matches!(
            result.status.unwrap_err(),
            RuntimeError::BlockhashNotFound
        ));
        assert_eq!(result.fee, 5000);
    }

    #[test]
    fn compute_exceeded() {
        let (storage, _dir) = test_storage();
        let kp = Keypair::generate();
        let from = kp.address();
        let to = hash(b"bob");

        storage
            .put_account(&from, 0, &Account::new(1_000_000, *SYSTEM_PROGRAM_ID))
            .unwrap();

        let set_limit = nusantara_compute_budget_program::set_compute_unit_limit(100);
        let transfer = nusantara_system_program::transfer(&from, &to, 100);
        let msg = Message::new(&[set_limit, transfer], &from).unwrap();
        let mut tx = Transaction::new(msg);
        tx.sign(&[&kp]);

        let sysvars = test_sysvars();
        let fee_calc = FeeCalculator::default();
        let cache = ProgramCache::new(16);
        let result = execute_transaction(&tx, &storage, &sysvars, &fee_calc, 1, &cache, None, false);

        assert!(result.status.is_err());
    }

    #[test]
    fn failure_still_deducts_fee() {
        let (storage, _dir) = test_storage();
        let kp = Keypair::generate();
        let from = kp.address();
        let to = hash(b"bob");

        storage
            .put_account(&from, 0, &Account::new(10_000, *SYSTEM_PROGRAM_ID))
            .unwrap();

        let tx = create_signed_transfer_tx(&kp, to, 1_000_000);
        let sysvars = test_sysvars();
        let fee_calc = FeeCalculator::default();
        let cache = ProgramCache::new(16);
        let result = execute_transaction(&tx, &storage, &sysvars, &fee_calc, 1, &cache, None, false);

        assert!(result.status.is_err());
        assert_eq!(result.fee, 5000);
        assert!(!result.account_deltas.is_empty());
        let payer_delta = result
            .account_deltas
            .iter()
            .find(|(addr, _)| addr == &from)
            .unwrap();
        assert_eq!(payer_delta.1.lamports, 5000);
    }

    #[test]
    fn multi_instruction() {
        let (storage, _dir) = test_storage();
        let kp = Keypair::generate();
        let alice = kp.address();
        let bob = hash(b"bob");
        let carol = hash(b"carol");

        storage
            .put_account(&alice, 0, &Account::new(1_000_000, *SYSTEM_PROGRAM_ID))
            .unwrap();
        storage
            .put_account(&bob, 0, &Account::new(500_000, *SYSTEM_PROGRAM_ID))
            .unwrap();

        let ix1 = nusantara_system_program::transfer(&alice, &bob, 100_000);
        let ix2 = nusantara_system_program::transfer(&alice, &carol, 50_000);
        let msg = Message::new(&[ix1, ix2], &alice).unwrap();
        let mut tx = Transaction::new(msg);
        tx.sign(&[&kp]);

        let sysvars = test_sysvars();
        let fee_calc = FeeCalculator::default();
        let cache = ProgramCache::new(16);
        let result = execute_transaction(&tx, &storage, &sysvars, &fee_calc, 1, &cache, None, false);

        assert!(result.status.is_ok());
    }

    #[test]
    fn compute_override() {
        let (storage, _dir) = test_storage();
        let kp = Keypair::generate();
        let from = kp.address();
        let to = hash(b"bob");

        storage
            .put_account(&from, 0, &Account::new(1_000_000, *SYSTEM_PROGRAM_ID))
            .unwrap();

        let set_limit = nusantara_compute_budget_program::set_compute_unit_limit(500_000);
        let transfer = nusantara_system_program::transfer(&from, &to, 100);
        let msg = Message::new(&[set_limit, transfer], &from).unwrap();
        let mut tx = Transaction::new(msg);
        tx.sign(&[&kp]);

        let sysvars = test_sysvars();
        let fee_calc = FeeCalculator::default();
        let cache = ProgramCache::new(16);
        let result = execute_transaction(&tx, &storage, &sysvars, &fee_calc, 1, &cache, None, false);

        assert!(result.status.is_ok());
    }

    #[test]
    fn readonly_preserved() {
        let (storage, _dir) = test_storage();
        let kp = Keypair::generate();
        let from = kp.address();
        let to = hash(b"bob");

        storage
            .put_account(&from, 0, &Account::new(1_000_000, *SYSTEM_PROGRAM_ID))
            .unwrap();
        storage
            .put_account(&to, 0, &Account::new(500_000, *SYSTEM_PROGRAM_ID))
            .unwrap();

        let tx = create_signed_transfer_tx(&kp, to, 100);
        let sysvars = test_sysvars();
        let fee_calc = FeeCalculator::default();
        let cache = ProgramCache::new(16);
        let result = execute_transaction(&tx, &storage, &sysvars, &fee_calc, 1, &cache, None, false);

        assert!(result.status.is_ok());
        for (addr, _) in &result.account_deltas {
            assert_ne!(addr, &*SYSTEM_PROGRAM_ID);
        }
    }

    #[test]
    fn lamports_conservation() {
        let (storage, _dir) = test_storage();
        let kp = Keypair::generate();
        let from = kp.address();
        let to = hash(b"bob");

        storage
            .put_account(&from, 0, &Account::new(1_000_000, *SYSTEM_PROGRAM_ID))
            .unwrap();
        storage
            .put_account(&to, 0, &Account::new(500_000, *SYSTEM_PROGRAM_ID))
            .unwrap();

        let tx = create_signed_transfer_tx(&kp, to, 100_000);
        let sysvars = test_sysvars();
        let fee_calc = FeeCalculator::default();
        let cache = ProgramCache::new(16);
        let result = execute_transaction(&tx, &storage, &sysvars, &fee_calc, 1, &cache, None, false);

        assert!(result.status.is_ok());

        let pre_total: u64 = result.pre_balances.iter().sum();
        let post_total: u64 = result.post_balances.iter().sum();
        assert_eq!(pre_total - result.fee, post_total);
    }

    #[test]
    fn unsigned_tx_rejected() {
        let (storage, _dir) = test_storage();
        let from = hash(b"alice");
        let to = hash(b"bob");

        storage
            .put_account(&from, 0, &Account::new(1_000_000, *SYSTEM_PROGRAM_ID))
            .unwrap();

        // Create unsigned transaction (no sign call)
        let ix = nusantara_system_program::transfer(&from, &to, 100);
        let msg = Message::new(&[ix], &from).unwrap();
        let tx = Transaction::new(msg);

        let sysvars = test_sysvars();
        let fee_calc = FeeCalculator::default();
        let cache = ProgramCache::new(16);
        let result = execute_transaction(&tx, &storage, &sysvars, &fee_calc, 1, &cache, None, false);

        assert!(result.status.is_err());
        assert!(matches!(
            result.status.unwrap_err(),
            RuntimeError::SignatureVerificationFailed(_)
        ));
        assert_eq!(result.fee, 0);
    }

    #[test]
    fn forged_signature_rejected() {
        let (storage, _dir) = test_storage();
        let kp1 = Keypair::generate();
        let kp2 = Keypair::generate();
        let from = kp1.address();
        let to = hash(b"bob");

        storage
            .put_account(&from, 0, &Account::new(1_000_000, *SYSTEM_PROGRAM_ID))
            .unwrap();

        // Sign with kp1 but replace pubkey with kp2's
        let mut tx = create_signed_transfer_tx(&kp1, to, 100);
        tx.signer_pubkeys = vec![kp2.public_key().clone()];

        let sysvars = test_sysvars();
        let fee_calc = FeeCalculator::default();
        let cache = ProgramCache::new(16);
        let result = execute_transaction(&tx, &storage, &sysvars, &fee_calc, 1, &cache, None, false);

        assert!(result.status.is_err());
        assert!(matches!(
            result.status.unwrap_err(),
            RuntimeError::SignatureVerificationFailed(_)
        ));
    }
}
