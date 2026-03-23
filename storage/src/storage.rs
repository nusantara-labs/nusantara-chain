use std::path::Path;
use std::sync::Arc;

use rocksdb::{DB, Options};

use crate::cf;
use crate::error::StorageError;
use crate::write_batch::{BatchOp, StorageWriteBatch};

#[derive(Clone)]
pub struct Storage {
    pub(crate) db: Arc<DB>,
}

impl Storage {
    /// Open (or create) the storage database at the given path.
    pub fn open(path: &Path) -> Result<Self, StorageError> {
        let mut db_opts = Options::default();
        db_opts.create_if_missing(true);
        db_opts.create_missing_column_families(true);

        // Global RocksDB tuning
        let parallelism = std::thread::available_parallelism()
            .map(|p| p.get() as i32)
            .unwrap_or(4);
        db_opts.increase_parallelism(parallelism);
        db_opts.set_max_background_jobs(cf::MAX_BACKGROUND_JOBS as i32);
        db_opts.set_bytes_per_sync(1_048_576);

        let cf_descs = cf::cf_descriptors();
        let db = DB::open_cf_descriptors(&db_opts, path, cf_descs)?;
        Ok(Self { db: Arc::new(db) })
    }

    /// Destroy the database at the given path.
    pub fn destroy(path: &Path) -> Result<(), StorageError> {
        DB::destroy(&Options::default(), path)?;
        Ok(())
    }

    /// Atomically write a batch of operations.
    #[tracing::instrument(skip(self, batch), level = "debug")]
    pub fn write(&self, batch: &StorageWriteBatch) -> Result<(), StorageError> {
        let mut wb = rocksdb::WriteBatch::default();
        for op in &batch.ops {
            match op {
                BatchOp::Put { cf, key, value } => {
                    let handle = self.cf_handle(cf)?;
                    wb.put_cf(&handle, key, value);
                }
                BatchOp::Delete { cf, key } => {
                    let handle = self.cf_handle(cf)?;
                    wb.delete_cf(&handle, key);
                }
            }
        }
        self.db.write(wb)?;
        Ok(())
    }

    /// Get a raw value from a column family.
    pub fn get_cf(&self, cf_name: &'static str, key: &[u8]) -> Result<Option<Vec<u8>>, StorageError> {
        let handle = self.cf_handle(cf_name)?;
        Ok(self.db.get_cf(&handle, key)?)
    }

    /// Put a raw key-value pair into a column family.
    pub fn put_cf(
        &self,
        cf_name: &'static str,
        key: &[u8],
        value: &[u8],
    ) -> Result<(), StorageError> {
        let handle = self.cf_handle(cf_name)?;
        self.db.put_cf(&handle, key, value)?;
        Ok(())
    }

    /// Delete a key from a column family.
    pub fn delete_cf(&self, cf_name: &'static str, key: &[u8]) -> Result<(), StorageError> {
        let handle = self.cf_handle(cf_name)?;
        self.db.delete_cf(&handle, key)?;
        Ok(())
    }

    /// Flush memtables and WAL to persistent SST files on disk.
    ///
    /// Flushes all column families, not just the default one.
    pub fn flush_all(&self) -> Result<(), StorageError> {
        for cf_name in cf::ALL_CF_NAMES {
            if let Some(handle) = self.db.cf_handle(cf_name) {
                self.db.flush_cf(&handle).map_err(StorageError::RocksDb)?;
            }
        }
        self.db.flush_wal(true).map_err(StorageError::RocksDb)?;
        Ok(())
    }

    pub(crate) fn cf_handle(&self, cf_name: &'static str) -> Result<&rocksdb::ColumnFamily, StorageError> {
        self.db
            .cf_handle(cf_name)
            .ok_or(StorageError::CfNotFound(cf_name))
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

    #[test]
    fn open_and_close() {
        let (storage, dir) = temp_storage();
        drop(storage);
        // Re-open should succeed
        let _storage = Storage::open(dir.path()).unwrap();
    }

    #[test]
    fn raw_put_get() {
        let (storage, _dir) = temp_storage();
        storage
            .put_cf(cf::CF_DEFAULT, b"key1", b"value1")
            .unwrap();
        let val = storage.get_cf(cf::CF_DEFAULT, b"key1").unwrap();
        assert_eq!(val, Some(b"value1".to_vec()));
    }

    #[test]
    fn raw_get_missing() {
        let (storage, _dir) = temp_storage();
        let val = storage.get_cf(cf::CF_DEFAULT, b"missing").unwrap();
        assert_eq!(val, None);
    }

    #[test]
    fn raw_delete() {
        let (storage, _dir) = temp_storage();
        storage.put_cf(cf::CF_DEFAULT, b"key", b"val").unwrap();
        storage.delete_cf(cf::CF_DEFAULT, b"key").unwrap();
        let val = storage.get_cf(cf::CF_DEFAULT, b"key").unwrap();
        assert_eq!(val, None);
    }

    #[test]
    fn write_batch_atomic() {
        let (storage, _dir) = temp_storage();
        let mut batch = StorageWriteBatch::new();
        batch.put(cf::CF_DEFAULT, b"k1".to_vec(), b"v1".to_vec());
        batch.put(cf::CF_DEFAULT, b"k2".to_vec(), b"v2".to_vec());
        storage.write(&batch).unwrap();

        assert_eq!(
            storage.get_cf(cf::CF_DEFAULT, b"k1").unwrap(),
            Some(b"v1".to_vec())
        );
        assert_eq!(
            storage.get_cf(cf::CF_DEFAULT, b"k2").unwrap(),
            Some(b"v2".to_vec())
        );
    }

    #[test]
    fn write_batch_with_delete() {
        let (storage, _dir) = temp_storage();
        storage.put_cf(cf::CF_DEFAULT, b"del", b"val").unwrap();

        let mut batch = StorageWriteBatch::new();
        batch.delete(cf::CF_DEFAULT, b"del".to_vec());
        batch.put(cf::CF_DEFAULT, b"new", b"newval".to_vec());
        storage.write(&batch).unwrap();

        assert_eq!(storage.get_cf(cf::CF_DEFAULT, b"del").unwrap(), None);
        assert_eq!(
            storage.get_cf(cf::CF_DEFAULT, b"new").unwrap(),
            Some(b"newval".to_vec())
        );
    }

    #[test]
    fn all_column_families_accessible() {
        let (storage, _dir) = temp_storage();
        for cf_name in cf::ALL_CF_NAMES {
            storage
                .put_cf(cf_name, b"test_key", b"test_val")
                .unwrap();
            let val = storage.get_cf(cf_name, b"test_key").unwrap();
            assert_eq!(val, Some(b"test_val".to_vec()));
        }
    }

    #[test]
    fn destroy_db() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("destroy_test");
        let storage = Storage::open(&path).unwrap();
        storage.put_cf(cf::CF_DEFAULT, b"k", b"v").unwrap();
        drop(storage);

        Storage::destroy(&path).unwrap();

        // Re-open should have no data
        let storage = Storage::open(&path).unwrap();
        assert_eq!(storage.get_cf(cf::CF_DEFAULT, b"k").unwrap(), None);
    }
}
