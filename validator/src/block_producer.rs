use std::sync::Arc;
use std::time::Instant;

use nusantara_consensus::bank::ConsensusBank;
use nusantara_consensus::poh::PohRecorder;
use nusantara_core::{Block, BlockHeader, EpochSchedule, FeeCalculator, Transaction};
use nusantara_crypto::{Hash, hashv};
use nusantara_rent_program::Rent;
use nusantara_runtime::{ProgramCache, SlotExecutionResult, execute_slot_parallel};
use nusantara_storage::{SlotMeta, Storage};
use tracing::{info, instrument};

use nusantara_consensus::bank::FrozenBankState;

use crate::helpers;
use crate::error::ValidatorError;

/// Deferred block storage operations that can run in the background.
pub struct PendingBlockStorage {
    pub slot_meta: SlotMeta,
    pub frozen: FrozenBankState,
}

pub struct BlockProducer {
    identity_address: Hash,
    storage: Arc<Storage>,
    bank: Arc<ConsensusBank>,
    poh: PohRecorder,
    epoch_schedule: EpochSchedule,
    fee_calculator: FeeCalculator,
    rent: Rent,
    parent_slot: u64,
    parent_hash: Hash,
    parent_bank_hash: Hash,
    program_cache: Arc<ProgramCache>,
}

impl BlockProducer {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        identity_address: Hash,
        storage: Arc<Storage>,
        bank: Arc<ConsensusBank>,
        initial_poh_hash: Hash,
        epoch_schedule: EpochSchedule,
        fee_calculator: FeeCalculator,
        rent: Rent,
        parent_slot: u64,
        parent_hash: Hash,
        parent_bank_hash: Hash,
        program_cache: Arc<ProgramCache>,
        hashes_per_tick: u64,
    ) -> Self {
        Self {
            identity_address,
            storage,
            bank,
            poh: PohRecorder::with_hashes_per_tick(initial_poh_hash, hashes_per_tick),
            epoch_schedule,
            fee_calculator,
            rent,
            parent_slot,
            parent_hash,
            parent_bank_hash,
            program_cache,
        }
    }

    #[instrument(skip_all, fields(slot = slot, tx_count = transactions.len()))]
    pub fn produce_block(
        &mut self,
        slot: u64,
        transactions: Vec<Transaction>,
    ) -> Result<(Block, SlotExecutionResult, PendingBlockStorage), ValidatorError> {
        let start = Instant::now();

        let timestamp = helpers::unix_timestamp_secs();

        // 1. Advance bank to current slot (updates Clock sysvar)
        self.bank.advance_slot(slot, timestamp);

        // 2. Build SysvarCache with current bank state
        let sysvars = helpers::build_sysvar_cache(&self.bank, &self.rent, &self.epoch_schedule);

        // 3. Execute slot via parallel runtime (Sealevel-style scheduling)
        let exec_result = execute_slot_parallel(
            slot,
            &transactions,
            &self.storage,
            &sysvars,
            &self.fee_calculator,
            &self.program_cache,
        )?;

        // 4. Update state Merkle tree with account deltas from execution
        self.bank.update_state_tree(&exec_result.account_deltas);
        let state_root = self.bank.state_root();

        // 5. Record transaction hashes in PoH
        for tx in &transactions {
            self.poh.record(&tx.hash());
        }

        // 6. Produce slot ticks (PoH chain)
        let _ticks = self.poh.produce_slot();
        let poh_hash = self.poh.current_hash();

        // 7. Compute merkle root of transaction hashes
        let merkle_root = helpers::compute_merkle_root(&transactions);

        // 8. Compute block hash
        let block_hash = hashv(&[
            self.parent_hash.as_bytes(),
            &slot.to_le_bytes(),
            poh_hash.as_bytes(),
        ]);

        // 9. Freeze bank (must happen before building header so bank_hash is available)
        let frozen = self.bank.freeze(
            slot,
            self.parent_slot,
            block_hash,
            &self.parent_bank_hash,
            &exec_result.account_delta_hash,
            exec_result.transactions_executed,
        );

        // 10. Build block
        let block = Block {
            header: BlockHeader {
                slot,
                parent_slot: self.parent_slot,
                parent_hash: self.parent_hash,
                block_hash,
                timestamp,
                validator: self.identity_address,
                transaction_count: transactions.len() as u64,
                merkle_root,
                poh_hash,
                bank_hash: frozen.bank_hash,
                state_root,
            },
            transactions,
            batches: Vec::new(),
        };

        // 11. Prepare pending block storage (deferred to async background)
        let slot_meta = SlotMeta {
            slot,
            parent_slot: self.parent_slot,
            block_time: Some(timestamp),
            num_data_shreds: 0,
            num_code_shreds: 0,
            is_connected: true,
            completed: true,
        };
        let pending_storage = PendingBlockStorage { slot_meta, frozen: frozen.clone() };

        // 12. Update consensus bank (in-memory only, fast)
        self.bank.record_slot_hash(slot, block_hash);

        // 13. Update parent pointers and reset PoH
        self.parent_slot = slot;
        self.parent_hash = block_hash;
        self.parent_bank_hash = frozen.bank_hash;
        self.poh.reset(block_hash);

        // Metrics
        let elapsed = start.elapsed();
        metrics::counter!("nusantara_blocks_produced").increment(1);
        metrics::counter!("nusantara_parallel_execution_blocks").increment(1);
        metrics::gauge!("nusantara_current_slot").set(slot as f64);
        metrics::histogram!("nusantara_block_time_ms").record(elapsed.as_millis() as f64);
        metrics::gauge!("nusantara_transactions_per_slot")
            .set(exec_result.transactions_executed as f64);
        metrics::gauge!("nusantara_state_tree_leaves")
            .set(self.bank.state_tree_len() as f64);

        info!(
            slot,
            txs = exec_result.transactions_executed,
            succeeded = exec_result.transactions_succeeded,
            failed = exec_result.transactions_failed,
            fees = exec_result.total_fees,
            time_ms = elapsed.as_millis() as u64,
            "block produced"
        );

        Ok((block, exec_result, pending_storage))
    }

    /// Update parent pointers after replaying a network-received block.
    pub fn set_parent(&mut self, slot: u64, hash: Hash, bank_hash: Hash) {
        self.parent_slot = slot;
        self.parent_hash = hash;
        self.parent_bank_hash = bank_hash;
        self.poh.reset(hash);
    }

    #[allow(dead_code)]
    pub fn parent_slot(&self) -> u64 {
        self.parent_slot
    }

    #[allow(dead_code)]
    pub fn fee_calculator(&self) -> &FeeCalculator {
        &self.fee_calculator
    }

    #[allow(dead_code)]
    pub fn rent(&self) -> &Rent {
        &self.rent
    }

    #[allow(dead_code)]
    pub fn epoch_schedule(&self) -> &EpochSchedule {
        &self.epoch_schedule
    }

    #[allow(dead_code)]
    pub fn bank(&self) -> &Arc<ConsensusBank> {
        &self.bank
    }
}
