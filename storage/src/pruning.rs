use tracing::info;

use crate::cf::{CF_BLOCKS, CF_CODE_SHREDS, CF_DATA_SHREDS, CF_SLOT_META};
use crate::decode;
use crate::error::StorageError;
use crate::keys::{slot_key, FULL_BLOCK_PREFIX};
use crate::storage::Storage;
use crate::transaction::TransactionStatusMeta;

impl Storage {
    /// Delete ledger data for all slots strictly below `min_slot`.
    ///
    /// Purges entries from CF_BLOCKS, CF_SLOT_META, CF_DATA_SHREDS, and
    /// CF_CODE_SHREDS. Account data (CF_ACCOUNTS, CF_ACCOUNT_INDEX) is
    /// intentionally preserved so historical account state remains queryable.
    ///
    /// All range deletes are staged in a single `rocksdb::WriteBatch` and
    /// committed atomically, so a crash mid-purge leaves storage unchanged.
    ///
    /// Uses RocksDB's `delete_range_cf` for efficient bulk deletion -- this
    /// marks a tombstone range in the LSM tree rather than issuing individual
    /// deletes, making it O(1) in the number of keys removed.
    #[tracing::instrument(skip(self), level = "info")]
    pub fn purge_slots_below(&self, min_slot: u64) -> Result<u64, StorageError> {
        if min_slot == 0 {
            return Ok(0);
        }

        // Range: [slot 0, min_slot) in big-endian key space.
        // slot_key(0) is the lowest possible 8-byte BE key.
        // slot_key(min_slot) is the exclusive upper bound.
        let start = slot_key(0);
        let end = slot_key(min_slot);

        // Stage all range deletes in a single WriteBatch for atomicity.
        // A crash after some but not all delete_range_cf calls would leave
        // inconsistent state; batching ensures all-or-nothing semantics.
        let mut wb = rocksdb::WriteBatch::default();

        // Column families with slot-keyed ledger data (8-byte BE keys).
        // Consensus metadata (CF_ROOTS, CF_BANK_HASHES, CF_SLOT_HASHES) is
        // intentionally excluded — deleting them would lose finalization state.
        let slot_keyed_cfs: &[&str] = &[CF_BLOCKS, CF_SLOT_META];
        for cf_name in slot_keyed_cfs {
            let cf = self
                .db
                .cf_handle(cf_name)
                .ok_or(StorageError::CfNotFound(cf_name))?;
            wb.delete_range_cf(cf, &start, &end);
        }

        // Shred CFs use 12-byte keys: slot(8 BE) ++ index(4 BE).
        // To cover all shred indices for slots < min_slot, the range is:
        //   [slot(0)++index(0), slot(min_slot)++index(0))
        let shred_start = [start.as_slice(), &[0u8; 4]].concat();
        let shred_end = [end.as_slice(), &[0u8; 4]].concat();

        let shred_cfs: &[&str] = &[CF_DATA_SHREDS, CF_CODE_SHREDS];
        for cf_name in shred_cfs {
            let cf = self
                .db
                .cf_handle(cf_name)
                .ok_or(StorageError::CfNotFound(cf_name))?;
            wb.delete_range_cf(cf, &shred_start, &shred_end);
        }

        // Also purge full-block entries stored in CF_DEFAULT with FULL_BLOCK_PREFIX.
        // Keys: "block_" ++ slot(8 BE). Range: prefix++slot(0) .. prefix++slot(min_slot).
        let block_start = [FULL_BLOCK_PREFIX, start.as_slice()].concat();
        let block_end = [FULL_BLOCK_PREFIX, end.as_slice()].concat();
        let cf_default = self
            .db
            .cf_handle(crate::cf::CF_DEFAULT)
            .ok_or(StorageError::CfNotFound(crate::cf::CF_DEFAULT))?;
        wb.delete_range_cf(cf_default, &block_start, &block_end);

        // Commit the entire batch atomically
        self.db.write(wb).map_err(StorageError::RocksDb)?;

        info!(min_slot, "purged ledger slots below threshold");

        Ok(min_slot)
    }

    /// Purge transaction data (CF_TRANSACTIONS and CF_ADDRESS_SIGNATURES)
    /// for slots below `min_slot`. Transaction data uses hash-based keys
    /// so we can't use range deletes — this is a separate, opt-in method.
    ///
    /// On deserialization error, the method propagates the error rather than
    /// silently skipping or treating the slot as 0 (which would delete all data).
    pub fn purge_transaction_data_below(&self, min_slot: u64) -> Result<u64, StorageError> {
        use crate::cf::{CF_ADDRESS_SIGNATURES, CF_TRANSACTIONS};
        use rocksdb::IteratorMode;

        if min_slot == 0 {
            return Ok(0);
        }

        let mut count = 0u64;

        // Purge CF_TRANSACTIONS: deserialize each value fully to extract the slot
        // field from TransactionStatusMeta. Raw byte-peeking is avoided because it
        // couples to struct layout and silently treats corrupted data as slot 0,
        // which would delete ALL transactions.
        let cf_tx = self
            .db
            .cf_handle(CF_TRANSACTIONS)
            .ok_or(StorageError::CfNotFound(CF_TRANSACTIONS))?;
        let mut batch = crate::write_batch::StorageWriteBatch::new();
        let iter = self.db.iterator_cf(cf_tx, IteratorMode::Start);
        for item in iter {
            let (key, value) = item.map_err(StorageError::RocksDb)?;
            let meta: TransactionStatusMeta = decode(&value)?;
            if meta.slot < min_slot {
                batch.delete(CF_TRANSACTIONS, key.to_vec());
                count += 1;
            }
        }

        // Purge CF_ADDRESS_SIGNATURES by slot embedded in key (bytes 64..72 BE).
        // Key shape: address(64) ++ slot(8 BE) ++ tx_index(4 BE) = 76 bytes total.
        let cf_addr = self
            .db
            .cf_handle(CF_ADDRESS_SIGNATURES)
            .ok_or(StorageError::CfNotFound(CF_ADDRESS_SIGNATURES))?;
        let iter = self.db.iterator_cf(cf_addr, IteratorMode::Start);
        for item in iter {
            let (key, _) = item.map_err(StorageError::RocksDb)?;
            if key.len() != 76 {
                return Err(StorageError::Corruption(
                    "address_signatures key has unexpected length".into(),
                ));
            }
            let slot = u64::from_be_bytes(
                key[64..72]
                    .try_into()
                    .map_err(|_| StorageError::Corruption("invalid slot bytes in address_sig key".into()))?,
            );
            if slot < min_slot {
                batch.delete(CF_ADDRESS_SIGNATURES, key.to_vec());
                count += 1;
            }
        }

        if !batch.is_empty() {
            self.write(&batch)?;
        }

        info!(min_slot, count, "purged transaction data below threshold");
        Ok(count)
    }

    /// Orchestrator: atomically purge all ledger and transaction data below
    /// `min_slot`. Callers should prefer this over invoking `purge_slots_below`
    /// and `purge_transaction_data_below` separately so that neither CF set is
    /// forgotten.
    pub fn purge_below(&self, min_slot: u64) -> Result<u64, StorageError> {
        let slot_count = self.purge_slots_below(min_slot)?;
        let tx_count = self.purge_transaction_data_below(min_slot)?;
        Ok(slot_count + tx_count)
    }

    /// Compact all column families that received range deletions during purge.
    ///
    /// RocksDB range-delete tombstones do not reclaim disk space immediately;
    /// `compact_range_cf` triggers an explicit compaction pass that actually
    /// removes the data. This is a heavyweight operation and should be called
    /// asynchronously or during off-peak periods.
    pub fn compact_after_purge(&self, cfs: &[&'static str]) -> Result<(), StorageError> {
        for &cf_name in cfs {
            let handle = self.cf_handle(cf_name)?;
            self.db
                .compact_range_cf(&handle, None::<&[u8]>, None::<&[u8]>);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use nusantara_crypto::hash;

    use super::*;
    use crate::SlotMeta;
    use crate::shred::DataShred;

    fn temp_storage() -> (Storage, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(dir.path()).unwrap();
        (storage, dir)
    }

    fn test_slot_meta(slot: u64) -> SlotMeta {
        SlotMeta {
            slot,
            parent_slot: slot.saturating_sub(1),
            block_time: Some(1000 + slot as i64),
            num_data_shreds: 1,
            num_code_shreds: 0,
            is_connected: true,
            completed: true,
        }
    }

    fn test_data_shred(slot: u64, index: u32) -> DataShred {
        DataShred {
            slot,
            index,
            parent_offset: 1,
            data: vec![0u8; 16],
            flags: 0,
        }
    }

    #[test]
    fn purge_zero_is_noop() {
        let (storage, _dir) = temp_storage();
        let result = storage.purge_slots_below(0).unwrap();
        assert_eq!(result, 0);
    }

    #[test]
    fn purge_removes_old_slot_meta() {
        let (storage, _dir) = temp_storage();

        // Insert slot metas at slots 1..=10
        for slot in 1..=10 {
            storage.put_slot_meta(&test_slot_meta(slot)).unwrap();
        }

        // Purge slots below 6
        storage.purge_slots_below(6).unwrap();

        // Slots 1-5 should be gone
        for slot in 1..=5 {
            assert!(
                storage.get_slot_meta(slot).unwrap().is_none(),
                "slot {slot} should have been purged"
            );
        }

        // Slots 6-10 should remain
        for slot in 6..=10 {
            assert!(
                storage.get_slot_meta(slot).unwrap().is_some(),
                "slot {slot} should still exist"
            );
        }
    }

    #[test]
    fn purge_removes_old_block_headers() {
        let (storage, _dir) = temp_storage();

        for slot in 1..=10 {
            let header = nusantara_core::BlockHeader {
                slot,
                parent_slot: slot.saturating_sub(1),
                parent_hash: hash(b"parent"),
                block_hash: hash(format!("block_{slot}").as_bytes()),
                timestamp: 1000 + slot as i64,
                validator: hash(b"validator"),
                transaction_count: 0,
                merkle_root: hash(b"merkle"),
                poh_hash: nusantara_crypto::Hash::zero(),
                bank_hash: nusantara_crypto::Hash::zero(),
                state_root: nusantara_crypto::Hash::zero(),
            };
            storage.put_block_header(&header).unwrap();
        }

        storage.purge_slots_below(5).unwrap();

        for slot in 1..=4 {
            assert!(storage.get_block_header(slot).unwrap().is_none());
        }
        for slot in 5..=10 {
            assert!(storage.get_block_header(slot).unwrap().is_some());
        }
    }

    #[test]
    fn purge_removes_old_shreds() {
        let (storage, _dir) = temp_storage();

        for slot in 1..=10 {
            storage.put_data_shred(&test_data_shred(slot, 0)).unwrap();
        }

        storage.purge_slots_below(5).unwrap();

        for slot in 1..=4 {
            assert!(storage.get_data_shred(slot, 0).unwrap().is_none());
        }
        for slot in 5..=10 {
            assert!(storage.get_data_shred(slot, 0).unwrap().is_some());
        }
    }

    #[test]
    fn purge_preserves_accounts() {
        let (storage, _dir) = temp_storage();

        let addr = hash(b"alice");
        let account = nusantara_core::Account::new(1000, hash(b"system"));

        // Store account at slot 2
        storage.put_account(&addr, 2, &account).unwrap();

        // Purge slots below 5
        storage.purge_slots_below(5).unwrap();

        // Account should still be accessible
        let loaded = storage.get_account(&addr).unwrap();
        assert!(loaded.is_some(), "accounts must survive pruning");
        assert_eq!(loaded.unwrap().lamports, 1000);
    }

    #[test]
    fn purge_transaction_data_below_correct_boundaries() {
        // Write 5 tx statuses across slots 1..=5; call purge below 3;
        // verify slots 1,2 deleted and slots 3,4,5 retained.
        use crate::transaction::{TransactionStatus, TransactionStatusMeta};
        let (storage, _dir) = temp_storage();

        let tx_hashes: Vec<_> = (1u64..=5).map(|i| hash(format!("tx_{i}").as_bytes())).collect();
        for (i, tx_hash) in tx_hashes.iter().enumerate() {
            let slot = (i as u64) + 1;
            let meta = TransactionStatusMeta {
                slot,
                status: TransactionStatus::Success,
                fee: 5000,
                pre_balances: vec![1000],
                post_balances: vec![995],
                compute_units_consumed: 200,
            };
            storage.put_transaction_status(tx_hash, &meta).unwrap();
        }

        let deleted = storage.purge_transaction_data_below(3).unwrap();
        assert_eq!(deleted, 2, "should have deleted tx at slots 1 and 2");

        // Slots 1,2 must be gone
        assert!(storage.get_transaction_status(&tx_hashes[0]).unwrap().is_none(), "slot 1 tx should be purged");
        assert!(storage.get_transaction_status(&tx_hashes[1]).unwrap().is_none(), "slot 2 tx should be purged");

        // Slots 3,4,5 must remain
        assert!(storage.get_transaction_status(&tx_hashes[2]).unwrap().is_some(), "slot 3 tx must survive");
        assert!(storage.get_transaction_status(&tx_hashes[3]).unwrap().is_some(), "slot 4 tx must survive");
        assert!(storage.get_transaction_status(&tx_hashes[4]).unwrap().is_some(), "slot 5 tx must survive");
    }

    #[test]
    fn purge_transaction_data_below_zero_is_noop() {
        use crate::transaction::{TransactionStatus, TransactionStatusMeta};
        let (storage, _dir) = temp_storage();
        let tx = hash(b"some_tx");
        let meta = TransactionStatusMeta {
            slot: 1,
            status: TransactionStatus::Success,
            fee: 0,
            pre_balances: vec![],
            post_balances: vec![],
            compute_units_consumed: 0,
        };
        storage.put_transaction_status(&tx, &meta).unwrap();
        let deleted = storage.purge_transaction_data_below(0).unwrap();
        assert_eq!(deleted, 0);
        assert!(storage.get_transaction_status(&tx).unwrap().is_some());
    }

    #[test]
    fn purge_preserves_roots() {
        let (storage, _dir) = temp_storage();
        for slot in 1..=10 {
            storage.set_root(slot).unwrap();
        }
        storage.purge_slots_below(6).unwrap();
        // ALL roots must survive pruning
        for slot in 1..=10 {
            assert!(
                storage.is_root(slot).unwrap(),
                "root {slot} must survive pruning"
            );
        }
    }
}
