use borsh::BorshDeserialize;
use nusantara_core::{Block, BlockHeader};
use rocksdb::{Direction, IteratorMode};
use tracing::instrument;

use crate::cf::{CF_BLOCKS, CF_DEFAULT};
use crate::error::StorageError;
use crate::keys::slot_key;
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
            Some(bytes) => {
                let header = BlockHeader::try_from_slice(&bytes)
                    .map_err(|e| StorageError::Deserialization(e.to_string()))?;
                Ok(Some(header))
            }
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

    /// Delete a block (header from CF_BLOCKS + full block from CF_DEFAULT)
    /// in a single atomic WriteBatch. No-op if the block doesn't exist.
    pub fn delete_block(&self, slot: u64) -> Result<(), StorageError> {
        let mut batch = crate::write_batch::StorageWriteBatch::new();
        batch.delete(CF_BLOCKS, slot_key(slot).to_vec());
        batch.delete(CF_DEFAULT, [b"block_".as_slice(), &slot_key(slot)].concat());
        self.write(&batch)
    }

    /// Store a full block (header + transactions) in a single atomic WriteBatch.
    /// The header is also stored separately in CF_BLOCKS for fast header-only queries.
    #[instrument(skip(self, block), fields(slot = block.header.slot), level = "debug")]
    pub fn put_block(&self, block: &Block) -> Result<(), StorageError> {
        let header_value =
            borsh::to_vec(&block.header).map_err(|e| StorageError::Serialization(e.to_string()))?;
        let block_key = [b"block_".as_slice(), &slot_key(block.header.slot)].concat();
        let block_value =
            borsh::to_vec(block).map_err(|e| StorageError::Serialization(e.to_string()))?;

        let mut batch = crate::write_batch::StorageWriteBatch::new();
        batch.put(CF_BLOCKS, slot_key(block.header.slot).to_vec(), header_value);
        batch.put(CF_DEFAULT, block_key, block_value);
        self.write(&batch)
    }

    /// Get a full block (header + transactions) by slot.
    #[instrument(skip(self), level = "debug")]
    pub fn get_block(&self, slot: u64) -> Result<Option<Block>, StorageError> {
        let key = [b"block_".as_slice(), &slot_key(slot)].concat();
        match self.get_cf(CF_DEFAULT, &key)? {
            Some(bytes) => {
                let block = Block::try_from_slice(&bytes)
                    .map_err(|e| StorageError::Deserialization(e.to_string()))?;
                Ok(Some(block))
            }
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
                continue;
            }
            let slot = u64::from_be_bytes(
                key.as_ref()
                    .try_into()
                    .map_err(|_| StorageError::Corruption("invalid slot key".into()))?,
            );
            if slot > end_slot {
                break;
            }
            let header = BlockHeader::try_from_slice(&value)
                .map_err(|e| StorageError::Deserialization(e.to_string()))?;
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
}
