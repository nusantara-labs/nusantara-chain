use borsh::{BorshDeserialize, BorshSerialize};
use nusantara_crypto::Hash;
use rocksdb::IteratorMode;

use crate::cf::CF_SNAPSHOTS;
use crate::error::StorageError;
use crate::keys::slot_key;
use crate::storage::Storage;

#[derive(Clone, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct SnapshotManifest {
    pub slot: u64,
    pub bank_hash: Hash,
    pub account_count: u64,
    pub timestamp: i64,
}

impl Storage {
    /// Store a snapshot manifest.
    pub fn put_snapshot(&self, manifest: &SnapshotManifest) -> Result<(), StorageError> {
        let key = slot_key(manifest.slot);
        let value =
            borsh::to_vec(manifest).map_err(|e| StorageError::Serialization(e.to_string()))?;
        self.put_cf(CF_SNAPSHOTS, &key, &value)
    }

    /// Get a snapshot manifest by slot.
    pub fn get_snapshot(&self, slot: u64) -> Result<Option<SnapshotManifest>, StorageError> {
        let key = slot_key(slot);
        match self.get_cf(CF_SNAPSHOTS, &key)? {
            Some(bytes) => {
                let manifest = SnapshotManifest::try_from_slice(&bytes)
                    .map_err(|e| StorageError::Deserialization(e.to_string()))?;
                Ok(Some(manifest))
            }
            None => Ok(None),
        }
    }

    /// Get the latest (highest slot) snapshot manifest.
    pub fn get_latest_snapshot(&self) -> Result<Option<SnapshotManifest>, StorageError> {
        let cf = self
            .db
            .cf_handle(CF_SNAPSHOTS)
            .ok_or(StorageError::CfNotFound(CF_SNAPSHOTS))?;

        let mut iter = self.db.iterator_cf(cf, IteratorMode::End);
        match iter.next() {
            Some(Ok((_, value))) => {
                let manifest = SnapshotManifest::try_from_slice(&value)
                    .map_err(|e| StorageError::Deserialization(e.to_string()))?;
                Ok(Some(manifest))
            }
            Some(Err(e)) => Err(StorageError::RocksDb(e)),
            None => Ok(None),
        }
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

    fn test_manifest(slot: u64) -> SnapshotManifest {
        SnapshotManifest {
            slot,
            bank_hash: hash(format!("bank_{slot}").as_bytes()),
            account_count: 1000 + slot,
            timestamp: 1234567890 + slot as i64,
        }
    }

    #[test]
    fn put_and_get_snapshot() {
        let (storage, _dir) = temp_storage();
        let manifest = test_manifest(100);

        storage.put_snapshot(&manifest).unwrap();
        let loaded = storage.get_snapshot(100).unwrap().unwrap();
        assert_eq!(loaded, manifest);
    }

    #[test]
    fn get_missing_snapshot() {
        let (storage, _dir) = temp_storage();
        assert_eq!(storage.get_snapshot(999).unwrap(), None);
    }

    #[test]
    fn get_latest_snapshot() {
        let (storage, _dir) = temp_storage();
        assert_eq!(storage.get_latest_snapshot().unwrap(), None);

        storage.put_snapshot(&test_manifest(10)).unwrap();
        storage.put_snapshot(&test_manifest(50)).unwrap();
        storage.put_snapshot(&test_manifest(30)).unwrap();

        let latest = storage.get_latest_snapshot().unwrap().unwrap();
        assert_eq!(latest.slot, 50);
    }

    #[test]
    fn borsh_roundtrip() {
        let manifest = test_manifest(42);
        let encoded = borsh::to_vec(&manifest).unwrap();
        let decoded: SnapshotManifest = borsh::from_slice(&encoded).unwrap();
        assert_eq!(manifest, decoded);
    }
}
