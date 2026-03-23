use tracing::info;

use crate::cf::{CF_BLOCKS, CF_CODE_SHREDS, CF_DATA_SHREDS, CF_SLOT_META};
use crate::error::StorageError;
use crate::keys::slot_key;
use crate::storage::Storage;

impl Storage {
    /// Delete ledger data for all slots strictly below `min_slot`.
    ///
    /// Purges entries from CF_BLOCKS, CF_SLOT_META, CF_DATA_SHREDS, and
    /// CF_CODE_SHREDS. Account data (CF_ACCOUNTS, CF_ACCOUNT_INDEX) is
    /// intentionally preserved so historical account state remains queryable.
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

        // Column families with slot-keyed ledger data (8-byte BE keys).
        // Consensus metadata (CF_ROOTS, CF_BANK_HASHES, CF_SLOT_HASHES) is
        // intentionally excluded — deleting them would lose finalization state.
        let slot_keyed_cfs: &[&str] = &[
            CF_BLOCKS,
            CF_SLOT_META,
        ];
        for cf_name in slot_keyed_cfs {
            let cf = self
                .db
                .cf_handle(cf_name)
                .ok_or(StorageError::CfNotFound(cf_name))?;
            self.db
                .delete_range_cf(cf, &start, &end)
                .map_err(StorageError::RocksDb)?;
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
            self.db
                .delete_range_cf(cf, &shred_start, &shred_end)
                .map_err(StorageError::RocksDb)?;
        }

        // Also purge full-block entries stored in CF_DEFAULT with "block_" prefix.
        // Keys: "block_" ++ slot(8 BE). Range: prefix++slot(0) .. prefix++slot(min_slot).
        let block_prefix = b"block_";
        let block_start = [block_prefix.as_slice(), &start].concat();
        let block_end = [block_prefix.as_slice(), &end].concat();
        // CF_DEFAULT doesn't need cf_handle — use the named handle
        let cf_default = self
            .db
            .cf_handle(crate::cf::CF_DEFAULT)
            .ok_or(StorageError::CfNotFound(crate::cf::CF_DEFAULT))?;
        self.db
            .delete_range_cf(cf_default, &block_start, &block_end)
            .map_err(StorageError::RocksDb)?;

        info!(min_slot, "purged ledger slots below threshold");

        Ok(min_slot)
    }

    /// Purge transaction data (CF_TRANSACTIONS and CF_ADDRESS_SIGNATURES)
    /// for slots below `min_slot`. Transaction data uses hash-based keys
    /// so we can't use range deletes — this is a separate, opt-in method.
    pub fn purge_transaction_data_below(&self, min_slot: u64) -> Result<u64, StorageError> {
        use crate::cf::{CF_ADDRESS_SIGNATURES, CF_TRANSACTIONS};
        use rocksdb::IteratorMode;

        if min_slot == 0 {
            return Ok(0);
        }

        let mut count = 0u64;

        // Purge CF_TRANSACTIONS by scanning and checking slot in value
        let cf_tx = self
            .db
            .cf_handle(CF_TRANSACTIONS)
            .ok_or(StorageError::CfNotFound(CF_TRANSACTIONS))?;
        let mut batch = crate::write_batch::StorageWriteBatch::new();
        let iter = self.db.iterator_cf(cf_tx, IteratorMode::Start);
        for item in iter {
            let (key, value) = item.map_err(StorageError::RocksDb)?;
            // TransactionStatusMeta starts with slot (u64 LE via borsh)
            if value.len() >= 8 {
                let slot = u64::from_le_bytes(
                    value[..8].try_into().unwrap_or([0; 8]),
                );
                if slot < min_slot {
                    batch.delete(CF_TRANSACTIONS, key.to_vec());
                    count += 1;
                }
            }
        }

        // Purge CF_ADDRESS_SIGNATURES by slot embedded in key (bytes 64..72 BE)
        let cf_addr = self
            .db
            .cf_handle(CF_ADDRESS_SIGNATURES)
            .ok_or(StorageError::CfNotFound(CF_ADDRESS_SIGNATURES))?;
        let iter = self.db.iterator_cf(cf_addr, IteratorMode::Start);
        for item in iter {
            let (key, _) = item.map_err(StorageError::RocksDb)?;
            if key.len() >= 76 {
                let slot = u64::from_be_bytes(
                    key[64..72].try_into().unwrap_or([0; 8]),
                );
                if slot < min_slot {
                    batch.delete(CF_ADDRESS_SIGNATURES, key.to_vec());
                    count += 1;
                }
            }
        }

        if !batch.is_empty() {
            self.write(&batch)?;
        }

        info!(min_slot, count, "purged transaction data below threshold");
        Ok(count)
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
