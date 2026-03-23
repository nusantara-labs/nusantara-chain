use nusantara_sysvar_program::Sysvar;

use crate::cf::CF_SYSVARS;
use crate::error::StorageError;
use crate::storage::Storage;

impl Storage {
    /// Store a sysvar. Key = sysvar ID hash.
    pub fn put_sysvar<S: Sysvar>(&self, sysvar: &S) -> Result<(), StorageError> {
        let id = S::id();
        let value =
            borsh::to_vec(sysvar).map_err(|e| StorageError::Serialization(e.to_string()))?;
        self.put_cf(CF_SYSVARS, id.as_bytes(), &value)
    }

    /// Get a sysvar by its type. Key = sysvar ID hash.
    pub fn get_sysvar<S: Sysvar>(&self) -> Result<Option<S>, StorageError> {
        let id = S::id();
        match self.get_cf(CF_SYSVARS, id.as_bytes())? {
            Some(bytes) => {
                let sysvar = S::try_from_slice(&bytes)
                    .map_err(|e| StorageError::Deserialization(e.to_string()))?;
                Ok(Some(sysvar))
            }
            None => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nusantara_sysvar_program::Clock;

    fn temp_storage() -> (Storage, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(dir.path()).unwrap();
        (storage, dir)
    }

    #[test]
    fn put_and_get_sysvar() {
        let (storage, _dir) = temp_storage();
        let clock = Clock {
            slot: 42,
            epoch: 1,
            unix_timestamp: 1234567890,
            leader_schedule_epoch: 2,
            epoch_start_timestamp: 1234500000,
        };

        storage.put_sysvar(&clock).unwrap();
        let loaded: Clock = storage.get_sysvar::<Clock>().unwrap().unwrap();
        assert_eq!(loaded.slot, 42);
        assert_eq!(loaded.epoch, 1);
        assert_eq!(loaded.unix_timestamp, 1234567890);
    }

    #[test]
    fn get_missing_sysvar() {
        let (storage, _dir) = temp_storage();
        let result = storage.get_sysvar::<Clock>().unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn update_sysvar() {
        let (storage, _dir) = temp_storage();
        let clock1 = Clock {
            slot: 1,
            epoch: 0,
            unix_timestamp: 1000,
            leader_schedule_epoch: 1,
            epoch_start_timestamp: 900,
        };
        storage.put_sysvar(&clock1).unwrap();

        let clock2 = Clock {
            slot: 100,
            epoch: 1,
            unix_timestamp: 2000,
            leader_schedule_epoch: 2,
            epoch_start_timestamp: 1900,
        };
        storage.put_sysvar(&clock2).unwrap();

        let loaded: Clock = storage.get_sysvar::<Clock>().unwrap().unwrap();
        assert_eq!(loaded.slot, 100);
    }
}
