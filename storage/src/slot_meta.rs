use borsh::{BorshDeserialize, BorshSerialize};

use crate::cf::CF_SLOT_META;
use crate::error::StorageError;
use crate::keys::slot_key;
use crate::storage::Storage;

#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct SlotMeta {
    pub slot: u64,
    pub parent_slot: u64,
    pub block_time: Option<i64>,
    pub num_data_shreds: u32,
    pub num_code_shreds: u32,
    pub is_connected: bool,
    pub completed: bool,
}

impl Storage {
    /// Store slot metadata.
    pub fn put_slot_meta(&self, meta: &SlotMeta) -> Result<(), StorageError> {
        let key = slot_key(meta.slot);
        let value =
            borsh::to_vec(meta).map_err(|e| StorageError::Serialization(e.to_string()))?;
        self.put_cf(CF_SLOT_META, &key, &value)
    }

    /// Get slot metadata by slot number.
    pub fn get_slot_meta(&self, slot: u64) -> Result<Option<SlotMeta>, StorageError> {
        let key = slot_key(slot);
        match self.get_cf(CF_SLOT_META, &key)? {
            Some(bytes) => {
                let meta = SlotMeta::try_from_slice(&bytes)
                    .map_err(|e| StorageError::Deserialization(e.to_string()))?;
                Ok(Some(meta))
            }
            None => Ok(None),
        }
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

    fn test_slot_meta(slot: u64) -> SlotMeta {
        SlotMeta {
            slot,
            parent_slot: slot.saturating_sub(1),
            block_time: Some(1000 + slot as i64),
            num_data_shreds: 10,
            num_code_shreds: 5,
            is_connected: true,
            completed: true,
        }
    }

    #[test]
    fn put_and_get_slot_meta() {
        let (storage, _dir) = temp_storage();
        let meta = test_slot_meta(42);

        storage.put_slot_meta(&meta).unwrap();
        let loaded = storage.get_slot_meta(42).unwrap().unwrap();
        assert_eq!(loaded, meta);
    }

    #[test]
    fn get_missing_slot_meta() {
        let (storage, _dir) = temp_storage();
        assert_eq!(storage.get_slot_meta(999).unwrap(), None);
    }

    #[test]
    fn borsh_roundtrip() {
        let meta = test_slot_meta(1);
        let encoded = borsh::to_vec(&meta).unwrap();
        let decoded: SlotMeta = borsh::from_slice(&encoded).unwrap();
        assert_eq!(meta, decoded);
    }

    #[test]
    fn slot_meta_with_no_block_time() {
        let (storage, _dir) = temp_storage();
        let meta = SlotMeta {
            slot: 1,
            parent_slot: 0,
            block_time: None,
            num_data_shreds: 0,
            num_code_shreds: 0,
            is_connected: false,
            completed: false,
        };
        storage.put_slot_meta(&meta).unwrap();
        let loaded = storage.get_slot_meta(1).unwrap().unwrap();
        assert_eq!(loaded, meta);
    }
}
