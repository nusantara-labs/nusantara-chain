use borsh::{BorshDeserialize, BorshSerialize};
use rocksdb::IteratorMode;

use crate::cf::{CF_CODE_SHREDS, CF_DATA_SHREDS};
use crate::error::StorageError;
use crate::keys::shred_key;
use crate::storage::Storage;

#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct DataShred {
    pub slot: u64,
    pub index: u32,
    pub parent_offset: u16,
    pub data: Vec<u8>,
    pub flags: u8,
}

#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct CodeShred {
    pub slot: u64,
    pub index: u32,
    pub num_data_shreds: u32,
    pub num_code_shreds: u32,
    pub position: u32,
    pub data: Vec<u8>,
}

impl Storage {
    /// Store a data shred.
    pub fn put_data_shred(&self, shred: &DataShred) -> Result<(), StorageError> {
        let key = shred_key(shred.slot, shred.index);
        let value =
            borsh::to_vec(shred).map_err(|e| StorageError::Serialization(e.to_string()))?;
        self.put_cf(CF_DATA_SHREDS, &key, &value)
    }

    /// Get a data shred by slot and index.
    pub fn get_data_shred(
        &self,
        slot: u64,
        index: u32,
    ) -> Result<Option<DataShred>, StorageError> {
        let key = shred_key(slot, index);
        match self.get_cf(CF_DATA_SHREDS, &key)? {
            Some(bytes) => {
                let shred = DataShred::try_from_slice(&bytes)
                    .map_err(|e| StorageError::Deserialization(e.to_string()))?;
                Ok(Some(shred))
            }
            None => Ok(None),
        }
    }

    /// Store a code (erasure) shred.
    pub fn put_code_shred(&self, shred: &CodeShred) -> Result<(), StorageError> {
        let key = shred_key(shred.slot, shred.index);
        let value =
            borsh::to_vec(shred).map_err(|e| StorageError::Serialization(e.to_string()))?;
        self.put_cf(CF_CODE_SHREDS, &key, &value)
    }

    /// Get a code shred by slot and index.
    pub fn get_code_shred(
        &self,
        slot: u64,
        index: u32,
    ) -> Result<Option<CodeShred>, StorageError> {
        let key = shred_key(slot, index);
        match self.get_cf(CF_CODE_SHREDS, &key)? {
            Some(bytes) => {
                let shred = CodeShred::try_from_slice(&bytes)
                    .map_err(|e| StorageError::Deserialization(e.to_string()))?;
                Ok(Some(shred))
            }
            None => Ok(None),
        }
    }

    /// Get all data shreds for a slot, ordered by index.
    pub fn get_data_shreds_for_slot(&self, slot: u64) -> Result<Vec<DataShred>, StorageError> {
        let cf = self
            .db
            .cf_handle(CF_DATA_SHREDS)
            .ok_or(StorageError::CfNotFound(CF_DATA_SHREDS))?;

        let prefix = slot.to_be_bytes();
        let iter = self.db.iterator_cf(
            cf,
            IteratorMode::From(&prefix, rocksdb::Direction::Forward),
        );

        let mut shreds = Vec::new();
        for item in iter {
            let (key, value) = item.map_err(StorageError::RocksDb)?;
            if key.len() < 8 || key[..8] != prefix {
                break;
            }
            let shred = DataShred::try_from_slice(&value)
                .map_err(|e| StorageError::Deserialization(e.to_string()))?;
            shreds.push(shred);
        }
        Ok(shreds)
    }

    /// Get all code shreds for a slot, ordered by index.
    pub fn get_code_shreds_for_slot(&self, slot: u64) -> Result<Vec<CodeShred>, StorageError> {
        let cf = self
            .db
            .cf_handle(CF_CODE_SHREDS)
            .ok_or(StorageError::CfNotFound(CF_CODE_SHREDS))?;

        let prefix = slot.to_be_bytes();
        let iter = self.db.iterator_cf(
            cf,
            IteratorMode::From(&prefix, rocksdb::Direction::Forward),
        );

        let mut shreds = Vec::new();
        for item in iter {
            let (key, value) = item.map_err(StorageError::RocksDb)?;
            if key.len() < 8 || key[..8] != prefix {
                break;
            }
            let shred = CodeShred::try_from_slice(&value)
                .map_err(|e| StorageError::Deserialization(e.to_string()))?;
            shreds.push(shred);
        }
        Ok(shreds)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_storage() -> (Storage, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(dir.path()).unwrap();
        (storage, dir)
    }

    fn test_data_shred(slot: u64, index: u32) -> DataShred {
        DataShred {
            slot,
            index,
            parent_offset: 1,
            data: vec![index as u8; 64],
            flags: 0,
        }
    }

    fn test_code_shred(slot: u64, index: u32) -> CodeShred {
        CodeShred {
            slot,
            index,
            num_data_shreds: 10,
            num_code_shreds: 5,
            position: index,
            data: vec![0xAB; 32],
        }
    }

    #[test]
    fn put_and_get_data_shred() {
        let (storage, _dir) = temp_storage();
        let shred = test_data_shred(1, 0);

        storage.put_data_shred(&shred).unwrap();
        let loaded = storage.get_data_shred(1, 0).unwrap().unwrap();
        assert_eq!(loaded, shred);
    }

    #[test]
    fn get_missing_data_shred() {
        let (storage, _dir) = temp_storage();
        assert_eq!(storage.get_data_shred(999, 0).unwrap(), None);
    }

    #[test]
    fn put_and_get_code_shred() {
        let (storage, _dir) = temp_storage();
        let shred = test_code_shred(1, 0);

        storage.put_code_shred(&shred).unwrap();
        let loaded = storage.get_code_shred(1, 0).unwrap().unwrap();
        assert_eq!(loaded, shred);
    }

    #[test]
    fn data_shreds_for_slot() {
        let (storage, _dir) = temp_storage();
        // Insert shreds for slot 1 and slot 2
        for i in 0..5 {
            storage.put_data_shred(&test_data_shred(1, i)).unwrap();
        }
        for i in 0..3 {
            storage.put_data_shred(&test_data_shred(2, i)).unwrap();
        }

        let slot1_shreds = storage.get_data_shreds_for_slot(1).unwrap();
        assert_eq!(slot1_shreds.len(), 5);
        for (i, shred) in slot1_shreds.iter().enumerate() {
            assert_eq!(shred.index, i as u32);
            assert_eq!(shred.slot, 1);
        }

        let slot2_shreds = storage.get_data_shreds_for_slot(2).unwrap();
        assert_eq!(slot2_shreds.len(), 3);
    }

    #[test]
    fn code_shreds_for_slot() {
        let (storage, _dir) = temp_storage();
        for i in 0..3 {
            storage.put_code_shred(&test_code_shred(5, i)).unwrap();
        }

        let shreds = storage.get_code_shreds_for_slot(5).unwrap();
        assert_eq!(shreds.len(), 3);
    }

    #[test]
    fn empty_slot_returns_empty() {
        let (storage, _dir) = temp_storage();
        assert!(storage.get_data_shreds_for_slot(1).unwrap().is_empty());
        assert!(storage.get_code_shreds_for_slot(1).unwrap().is_empty());
    }

    #[test]
    fn borsh_roundtrip_data_shred() {
        let shred = test_data_shred(10, 3);
        let encoded = borsh::to_vec(&shred).unwrap();
        let decoded: DataShred = borsh::from_slice(&encoded).unwrap();
        assert_eq!(shred, decoded);
    }

    #[test]
    fn borsh_roundtrip_code_shred() {
        let shred = test_code_shred(10, 3);
        let encoded = borsh::to_vec(&shred).unwrap();
        let decoded: CodeShred = borsh::from_slice(&encoded).unwrap();
        assert_eq!(shred, decoded);
    }
}
