use nusantara_crypto::Hash;
use rocksdb::IteratorMode;

use crate::cf::{CF_BANK_HASHES, CF_ROOTS, CF_SLOT_HASHES};
use crate::error::StorageError;
use crate::keys::slot_key;
use crate::storage::Storage;

impl Storage {
    /// Mark a slot as a finalized root.
    pub fn set_root(&self, slot: u64) -> Result<(), StorageError> {
        let key = slot_key(slot);
        self.put_cf(CF_ROOTS, &key, &[])
    }

    /// Check if a slot is a finalized root.
    pub fn is_root(&self, slot: u64) -> Result<bool, StorageError> {
        let key = slot_key(slot);
        Ok(self.get_cf(CF_ROOTS, &key)?.is_some())
    }

    /// Get the latest (highest) root slot.
    pub fn get_latest_root(&self) -> Result<Option<u64>, StorageError> {
        let cf = self
            .db
            .cf_handle(CF_ROOTS)
            .ok_or(StorageError::CfNotFound(CF_ROOTS))?;

        let mut iter = self.db.iterator_cf(cf, IteratorMode::End);
        match iter.next() {
            Some(Ok((key, _))) => {
                let slot = u64::from_be_bytes(
                    key.as_ref()
                        .try_into()
                        .map_err(|_| StorageError::Corruption("invalid root key".into()))?,
                );
                Ok(Some(slot))
            }
            Some(Err(e)) => Err(StorageError::RocksDb(e)),
            None => Ok(None),
        }
    }

    /// Store the bank hash for a slot.
    pub fn put_bank_hash(&self, slot: u64, hash: &Hash) -> Result<(), StorageError> {
        let key = slot_key(slot);
        self.put_cf(CF_BANK_HASHES, &key, hash.as_bytes())
    }

    /// Get the bank hash for a slot.
    pub fn get_bank_hash(&self, slot: u64) -> Result<Option<Hash>, StorageError> {
        let key = slot_key(slot);
        match self.get_cf(CF_BANK_HASHES, &key)? {
            Some(bytes) => {
                let arr: [u8; 64] = bytes
                    .try_into()
                    .map_err(|_| StorageError::Corruption("invalid bank hash length".into()))?;
                Ok(Some(Hash::new(arr)))
            }
            None => Ok(None),
        }
    }

    /// Store the slot-to-block-hash mapping.
    pub fn put_slot_hash(&self, slot: u64, hash: &Hash) -> Result<(), StorageError> {
        let key = slot_key(slot);
        self.put_cf(CF_SLOT_HASHES, &key, hash.as_bytes())
    }

    /// Get the block hash for a slot.
    pub fn get_slot_hash(&self, slot: u64) -> Result<Option<Hash>, StorageError> {
        let key = slot_key(slot);
        match self.get_cf(CF_SLOT_HASHES, &key)? {
            Some(bytes) => {
                let arr: [u8; 64] = bytes
                    .try_into()
                    .map_err(|_| StorageError::Corruption("invalid slot hash length".into()))?;
                Ok(Some(Hash::new(arr)))
            }
            None => Ok(None),
        }
    }

    /// Get recent slot hashes at or below `max_slot`, up to `limit` entries.
    /// Returns entries in descending slot order: `[(slot_n, hash_n), ...]`.
    /// Used to backfill slot_hashes beyond the fork tree's pruned ancestry.
    pub fn get_recent_slot_hashes_below(
        &self,
        max_slot: u64,
        limit: usize,
    ) -> Result<Vec<(u64, Hash)>, StorageError> {
        let cf = self
            .db
            .cf_handle(CF_SLOT_HASHES)
            .ok_or(StorageError::CfNotFound(CF_SLOT_HASHES))?;

        let start_key = slot_key(max_slot);
        let iter = self.db.iterator_cf(
            cf,
            IteratorMode::From(&start_key, rocksdb::Direction::Reverse),
        );

        let mut result = Vec::with_capacity(limit.min(512));
        for item in iter {
            if result.len() >= limit {
                break;
            }
            let (key, value) = item.map_err(StorageError::RocksDb)?;
            let slot = u64::from_be_bytes(
                key.as_ref()
                    .try_into()
                    .map_err(|_| StorageError::Corruption("invalid slot key".into()))?,
            );
            let arr: [u8; 64] = value
                .as_ref()
                .try_into()
                .map_err(|_| StorageError::Corruption("invalid slot hash length".into()))?;
            result.push((slot, Hash::new(arr)));
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nusantara_crypto::hash;

    fn temp_storage() -> (Storage, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(dir.path()).unwrap();
        (storage, dir)
    }

    #[test]
    fn roots() {
        let (storage, _dir) = temp_storage();
        assert!(!storage.is_root(1).unwrap());
        assert_eq!(storage.get_latest_root().unwrap(), None);

        storage.set_root(5).unwrap();
        storage.set_root(10).unwrap();
        storage.set_root(3).unwrap();

        assert!(storage.is_root(5).unwrap());
        assert!(storage.is_root(10).unwrap());
        assert!(storage.is_root(3).unwrap());
        assert!(!storage.is_root(7).unwrap());

        assert_eq!(storage.get_latest_root().unwrap(), Some(10));
    }

    #[test]
    fn bank_hashes() {
        let (storage, _dir) = temp_storage();
        let h = hash(b"bank_42");

        assert_eq!(storage.get_bank_hash(42).unwrap(), None);

        storage.put_bank_hash(42, &h).unwrap();
        let loaded = storage.get_bank_hash(42).unwrap().unwrap();
        assert_eq!(loaded, h);
    }

    #[test]
    fn slot_hashes() {
        let (storage, _dir) = temp_storage();
        let h = hash(b"slot_1");

        assert_eq!(storage.get_slot_hash(1).unwrap(), None);

        storage.put_slot_hash(1, &h).unwrap();
        let loaded = storage.get_slot_hash(1).unwrap().unwrap();
        assert_eq!(loaded, h);
    }

    #[test]
    fn recent_slot_hashes_below() {
        let (storage, _dir) = temp_storage();

        // Store hashes for slots 1..=10
        let hashes: Vec<Hash> = (1..=10).map(|i| hash(format!("slot_{i}").as_bytes())).collect();
        for (i, h) in hashes.iter().enumerate() {
            storage.put_slot_hash((i + 1) as u64, h).unwrap();
        }

        // Get 5 entries at or below slot 8
        let result = storage.get_recent_slot_hashes_below(8, 5).unwrap();
        assert_eq!(result.len(), 5);
        assert_eq!(result[0].0, 8);
        assert_eq!(result[1].0, 7);
        assert_eq!(result[4].0, 4);
        assert_eq!(result[0].1, hashes[7]);

        // Get all entries at or below slot 3
        let result = storage.get_recent_slot_hashes_below(3, 100).unwrap();
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].0, 3);
        assert_eq!(result[2].0, 1);

        // Empty result for slot 0
        let result = storage.get_recent_slot_hashes_below(0, 10).unwrap();
        assert!(result.is_empty());
    }
}
