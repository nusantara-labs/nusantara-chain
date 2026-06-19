use nusantara_core::{Block, BlockHeader};
use rocksdb::{Direction, IteratorMode};
use tracing::instrument;

use crate::cf::{CF_ADDRESS_SIGNATURES, CF_BLOCKS, CF_DEFAULT, CF_SLOT_META, CF_TRANSACTIONS};
use crate::decode;
use crate::error::StorageError;
use crate::keys::{address_sig_key, full_block_key, slot_key};
use crate::storage::Storage;

impl Storage {
    /// Store a block header.
    #[instrument(skip(self, header), fields(slot = header.slot), level = "debug")]
    pub fn put_block_header(&self, header: &BlockHeader) -> Result<(), StorageError> {
        let key = slot_key(header.slot);
        let value =
            borsh::to_vec(header).map_err(|e| StorageError::Serialization(e.to_string()))?;
        self.put_cf(CF_BLOCKS, &key, &value)
    }

    /// Get a block header by slot.
    pub fn get_block_header(&self, slot: u64) -> Result<Option<BlockHeader>, StorageError> {
        let key = slot_key(slot);
        match self.get_cf(CF_BLOCKS, &key)? {
            Some(bytes) => Ok(Some(decode::<BlockHeader>(&bytes)?)),
            None => Ok(None),
        }
    }

    /// Check if a block header exists for a slot (without deserializing).
    /// Uses `get_pinned_cf` to avoid copying the value — only checks existence.
    #[instrument(skip(self), level = "debug")]
    pub fn has_block_header(&self, slot: u64) -> Result<bool, StorageError> {
        let key = slot_key(slot);
        let cf = self.cf_handle(CF_BLOCKS)?;
        Ok(self.db.get_pinned_cf(cf, key)?.is_some())
    }

    /// Delete a block and all its secondary index entries atomically.
    ///
    /// Stages deletes for: CF_BLOCKS (header), CF_DEFAULT (full block),
    /// CF_TRANSACTIONS (per-tx status), CF_ADDRESS_SIGNATURES (per-tx signers),
    /// and CF_SLOT_META — all in one `StorageWriteBatch`. A crash before commit
    /// leaves storage unchanged; after commit, all orphaned entries are removed.
    ///
    /// No-op if the block doesn't exist.
    pub fn delete_block(&self, slot: u64) -> Result<(), StorageError> {
        let slot_key_bytes = slot_key(slot);
        let block_cf_key = full_block_key(slot);

        let mut batch = crate::write_batch::StorageWriteBatch::new();

        // Stage CF_BLOCKS and CF_DEFAULT deletes unconditionally (RocksDB delete
        // is a no-op when the key does not exist).
        batch.delete(CF_BLOCKS, slot_key_bytes.to_vec());
        batch.delete(CF_DEFAULT, block_cf_key.to_vec());

        // Stage CF_SLOT_META delete
        batch.delete(CF_SLOT_META, slot_key_bytes.to_vec());

        // Load the full block to discover which transactions and signers need
        // index cleanup. If the block doesn't exist, the header/block deletes
        // above are no-ops and we're done.
        if let Some(block) = self.get_block(slot)? {
            for (tx_index, tx) in block.transactions.iter().enumerate() {
                // Remove the transaction status entry
                batch.delete(CF_TRANSACTIONS, tx.hash().as_bytes().to_vec());

                // Remove address-signature index entries for every account in the tx.
                // The account_keys list matches the key shape used in put_address_signature.
                for address in &tx.message.account_keys {
                    let key = address_sig_key(address, slot, tx_index as u32);
                    batch.delete(CF_ADDRESS_SIGNATURES, key.to_vec());
                }
            }
        }

        self.write(&batch)
    }

    /// Store a full block (header + transactions) in a single atomic WriteBatch.
    /// The header is also stored separately in CF_BLOCKS for fast header-only queries.
    #[instrument(skip(self, block), fields(slot = block.header.slot), level = "debug")]
    pub fn put_block(&self, block: &Block) -> Result<(), StorageError> {
        let header_value =
            borsh::to_vec(&block.header).map_err(|e| StorageError::Serialization(e.to_string()))?;
        let block_key = full_block_key(block.header.slot);
        let block_value =
            borsh::to_vec(block).map_err(|e| StorageError::Serialization(e.to_string()))?;

        let mut batch = crate::write_batch::StorageWriteBatch::new();
        batch.put(CF_BLOCKS, slot_key(block.header.slot).to_vec(), header_value);
        batch.put(CF_DEFAULT, block_key.to_vec(), block_value);
        self.write(&batch)
    }

    /// Get a full block (header + transactions) by slot.
    #[instrument(skip(self), level = "debug")]
    pub fn get_block(&self, slot: u64) -> Result<Option<Block>, StorageError> {
        let key = full_block_key(slot);
        match self.get_cf(CF_DEFAULT, &key)? {
            Some(bytes) => Ok(Some(decode::<Block>(&bytes)?)),
            None => Ok(None),
        }
    }

    /// Get the latest (highest) slot that has a block header.
    pub fn get_latest_slot(&self) -> Result<Option<u64>, StorageError> {
        let cf = self
            .db
            .cf_handle(CF_BLOCKS)
            .ok_or(StorageError::CfNotFound(CF_BLOCKS))?;

        let mut iter = self.db.iterator_cf(cf, IteratorMode::End);
        match iter.next() {
            Some(Ok((key, _))) => {
                if key.len() != 8 {
                    return Ok(None);
                }
                let slot = u64::from_be_bytes(
                    key.as_ref()
                        .try_into()
                        .map_err(|_| StorageError::Corruption("invalid slot key".into()))?,
                );
                Ok(Some(slot))
            }
            Some(Err(e)) => Err(StorageError::RocksDb(e)),
            None => Ok(None),
        }
    }

    /// Get block headers in a slot range (inclusive).
    pub fn get_block_headers_range(
        &self,
        start_slot: u64,
        end_slot: u64,
    ) -> Result<Vec<BlockHeader>, StorageError> {
        let cf = self
            .db
            .cf_handle(CF_BLOCKS)
            .ok_or(StorageError::CfNotFound(CF_BLOCKS))?;

        let start_key = slot_key(start_slot);
        let iter = self
            .db
            .iterator_cf(cf, IteratorMode::From(&start_key, Direction::Forward));

        let mut headers = Vec::new();
        for item in iter {
            let (key, value) = item.map_err(StorageError::RocksDb)?;
            if key.len() != 8 {
                return Err(StorageError::Corruption(
                    "blocks CF key has unexpected length".into(),
                ));
            }
            let slot = u64::from_be_bytes(
                key.as_ref()
                    .try_into()
                    .map_err(|_| StorageError::Corruption("invalid slot key".into()))?,
            );
            if slot > end_slot {
                break;
            }
            let header = decode::<BlockHeader>(&value)?;
            headers.push(header);
        }
        Ok(headers)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nusantara_crypto::{Hash, hash};

    fn temp_storage() -> (Storage, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(dir.path()).unwrap();
        (storage, dir)
    }

    fn test_header(slot: u64) -> BlockHeader {
        BlockHeader {
            slot,
            parent_slot: slot.saturating_sub(1),
            parent_hash: hash(b"parent"),
            block_hash: hash(format!("block_{slot}").as_bytes()),
            timestamp: 1000 + slot as i64,
            validator: hash(b"validator"),
            transaction_count: 5,
            merkle_root: hash(b"merkle"),
            poh_hash: Hash::zero(),
            bank_hash: Hash::zero(),
            state_root: Hash::zero(),
        }
    }

    #[test]
    fn put_and_get_block_header() {
        let (storage, _dir) = temp_storage();
        let header = test_header(42);

        storage.put_block_header(&header).unwrap();
        let loaded = storage.get_block_header(42).unwrap().unwrap();
        assert_eq!(loaded, header);
    }

    #[test]
    fn get_missing_block_header() {
        let (storage, _dir) = temp_storage();
        assert_eq!(storage.get_block_header(999).unwrap(), None);
    }

    #[test]
    fn get_latest_slot() {
        let (storage, _dir) = temp_storage();
        assert_eq!(storage.get_latest_slot().unwrap(), None);

        storage.put_block_header(&test_header(10)).unwrap();
        storage.put_block_header(&test_header(20)).unwrap();
        storage.put_block_header(&test_header(5)).unwrap();

        assert_eq!(storage.get_latest_slot().unwrap(), Some(20));
    }

    #[test]
    fn block_headers_range() {
        let (storage, _dir) = temp_storage();
        for slot in [1, 3, 5, 7, 9] {
            storage.put_block_header(&test_header(slot)).unwrap();
        }

        let headers = storage.get_block_headers_range(3, 7).unwrap();
        assert_eq!(headers.len(), 3);
        assert_eq!(headers[0].slot, 3);
        assert_eq!(headers[1].slot, 5);
        assert_eq!(headers[2].slot, 7);
    }

    #[test]
    fn put_block() {
        let (storage, _dir) = temp_storage();
        let block = Block {
            header: test_header(1),
            transactions: Vec::new(),
            batches: Vec::new(),
        };
        storage.put_block(&block).unwrap();
        let loaded = storage.get_block_header(1).unwrap().unwrap();
        assert_eq!(loaded, block.header);
    }

    #[test]
    fn has_block_header() {
        let (storage, _dir) = temp_storage();
        assert!(!storage.has_block_header(42).unwrap());
        storage.put_block_header(&test_header(42)).unwrap();
        assert!(storage.has_block_header(42).unwrap());
    }

    #[test]
    fn delete_block() {
        let (storage, _dir) = temp_storage();
        let block = Block {
            header: test_header(10),
            transactions: Vec::new(),
            batches: Vec::new(),
        };
        storage.put_block(&block).unwrap();
        assert!(storage.has_block_header(10).unwrap());
        assert!(storage.get_block(10).unwrap().is_some());

        storage.delete_block(10).unwrap();
        assert!(!storage.has_block_header(10).unwrap());
        assert!(storage.get_block(10).unwrap().is_none());
    }

    #[test]
    fn delete_nonexistent_block_is_noop() {
        let (storage, _dir) = temp_storage();
        // Should not error when deleting a block that doesn't exist
        storage.delete_block(999).unwrap();
    }

    #[test]
    fn delete_block_cleans_secondary_indexes() {
        // Verify that delete_block removes CF_TRANSACTIONS, CF_ADDRESS_SIGNATURES,
        // and CF_SLOT_META orphaned entries in addition to CF_BLOCKS / CF_DEFAULT.
        use nusantara_core::{
            Transaction,
            instruction::{AccountMeta, Instruction},
            message::Message,
        };
        use crate::slot_meta::SlotMeta;
        use crate::transaction::{TransactionStatus, TransactionStatusMeta};

        let (storage, _dir) = temp_storage();
        let slot = 7u64;

        // Build 3 minimal transactions with distinct accounts
        let addr0 = hash(b"payer_0");
        let addr1 = hash(b"payer_1");
        let addr2 = hash(b"payer_2");
        let program = hash(b"system");

        let make_tx = |payer: Hash| -> nusantara_core::Transaction {
            let ix = Instruction {
                program_id: program,
                accounts: vec![AccountMeta::new(hash(b"target"), false)],
                data: vec![],
            };
            let msg = Message::new(&[ix], &payer).unwrap();
            Transaction::new(msg)
        };

        let tx0 = make_tx(addr0);
        let tx1 = make_tx(addr1);
        let tx2 = make_tx(addr2);

        // Store the block
        let block = Block {
            header: test_header(slot),
            transactions: vec![tx0.clone(), tx1.clone(), tx2.clone()],
            batches: Vec::new(),
        };
        storage.put_block(&block).unwrap();

        // Store slot meta
        let slot_meta = SlotMeta {
            slot,
            parent_slot: slot - 1,
            block_time: Some(9999),
            num_data_shreds: 1,
            num_code_shreds: 0,
            is_connected: true,
            completed: true,
        };
        storage.put_slot_meta(&slot_meta).unwrap();

        // Store transaction statuses and address-signature index entries
        let make_meta = |s: u64| TransactionStatusMeta {
            slot: s,
            status: TransactionStatus::Success,
            fee: 5000,
            pre_balances: vec![],
            post_balances: vec![],
            compute_units_consumed: 100,
        };
        storage.put_transaction_status(&tx0.hash(), &make_meta(slot)).unwrap();
        storage.put_transaction_status(&tx1.hash(), &make_meta(slot)).unwrap();
        storage.put_transaction_status(&tx2.hash(), &make_meta(slot)).unwrap();

        // Index address→tx for each transaction's first account
        storage.put_address_signature(&addr0, slot, 0, &tx0.hash()).unwrap();
        storage.put_address_signature(&addr1, slot, 1, &tx1.hash()).unwrap();
        storage.put_address_signature(&addr2, slot, 2, &tx2.hash()).unwrap();

        // Sanity: everything exists before deletion
        assert!(storage.get_block(slot).unwrap().is_some());
        assert!(storage.get_slot_meta(slot).unwrap().is_some());
        assert!(storage.get_transaction_status(&tx0.hash()).unwrap().is_some());

        // Delete the block
        storage.delete_block(slot).unwrap();

        // Block and slot meta must be gone
        assert!(!storage.has_block_header(slot).unwrap(), "header must be deleted");
        assert!(storage.get_block(slot).unwrap().is_none(), "full block must be deleted");
        assert!(storage.get_slot_meta(slot).unwrap().is_none(), "slot_meta must be deleted");

        // Transaction statuses must be gone
        assert!(storage.get_transaction_status(&tx0.hash()).unwrap().is_none(), "tx0 status must be deleted");
        assert!(storage.get_transaction_status(&tx1.hash()).unwrap().is_none(), "tx1 status must be deleted");
        assert!(storage.get_transaction_status(&tx2.hash()).unwrap().is_none(), "tx2 status must be deleted");

        // Address-signature entries must be gone
        let sigs0 = storage.get_signatures_for_address(&addr0, 10).unwrap();
        assert!(sigs0.is_empty(), "addr0 address_sig entries must be deleted");
        let sigs1 = storage.get_signatures_for_address(&addr1, 10).unwrap();
        assert!(sigs1.is_empty(), "addr1 address_sig entries must be deleted");
    }
}
