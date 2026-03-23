use std::collections::HashSet;

use borsh::BorshDeserialize;
use nusantara_core::Account;
use nusantara_crypto::Hash;
use rocksdb::IteratorMode;

use crate::cf::{CF_ACCOUNTS, CF_ACCOUNT_INDEX};
use crate::error::StorageError;
use crate::keys::{account_key, slot_key};
use crate::storage::Storage;
use crate::write_batch::StorageWriteBatch;

impl Storage {
    /// Store an account at a specific slot.
    /// Writes to `accounts` CF (historical), `account_index` CF (latest pointer),
    /// and owner/program indexes in a single atomic WriteBatch.
    #[tracing::instrument(skip(self, account), fields(address = %address, slot), level = "debug")]
    pub fn put_account(
        &self,
        address: &Hash,
        slot: u64,
        account: &Account,
    ) -> Result<(), StorageError> {
        let batch = self.prepare_account_write(address, slot, account)?;
        self.write(&batch)?;
        Ok(())
    }

    /// Prepare account write operations into a `StorageWriteBatch` WITHOUT committing.
    /// Returns a batch with CF_ACCOUNTS + CF_ACCOUNT_INDEX + owner/program index updates.
    /// The caller can merge this into a larger batch for amortized commits.
    pub fn prepare_account_write(
        &self,
        address: &Hash,
        slot: u64,
        account: &Account,
    ) -> Result<StorageWriteBatch, StorageError> {
        let mut batch = StorageWriteBatch::new();
        self.append_account_write(&mut batch, address, slot, account)?;
        Ok(batch)
    }

    /// Append account write operations directly into the caller's batch.
    /// Reads the old account from storage for owner index tracking.
    pub fn append_account_write(
        &self,
        batch: &mut StorageWriteBatch,
        address: &Hash,
        slot: u64,
        account: &Account,
    ) -> Result<(), StorageError> {
        let old_account = self.get_account(address)?;
        Self::write_account_to_batch(batch, address, slot, account, old_account.as_ref())?;
        Ok(())
    }

    /// Append account write operations using a caller-provided old account state,
    /// skipping the redundant `get_account()` RocksDB read.
    pub fn append_account_write_with_old(
        batch: &mut StorageWriteBatch,
        address: &Hash,
        slot: u64,
        account: &Account,
        old_account: Option<&Account>,
    ) -> Result<(), StorageError> {
        Self::write_account_to_batch(batch, address, slot, account, old_account)
    }

    /// Shared logic: serialize account and append CF_ACCOUNTS + CF_ACCOUNT_INDEX + index updates.
    fn write_account_to_batch(
        batch: &mut StorageWriteBatch,
        address: &Hash,
        slot: u64,
        account: &Account,
        old_account: Option<&Account>,
    ) -> Result<(), StorageError> {
        let value =
            borsh::to_vec(account).map_err(|e| StorageError::Serialization(e.to_string()))?;
        batch.put(CF_ACCOUNTS, account_key(address, slot).to_vec(), value);
        batch.put(CF_ACCOUNT_INDEX, address.as_bytes().to_vec(), slot_key(slot).to_vec());

        Self::write_index_updates(batch, address, old_account, account);
        Ok(())
    }

    /// Get the latest version of an account.
    #[tracing::instrument(skip(self), fields(address = %address), level = "debug")]
    pub fn get_account(&self, address: &Hash) -> Result<Option<Account>, StorageError> {
        let slot_bytes = match self.get_cf(CF_ACCOUNT_INDEX, address.as_bytes())? {
            Some(bytes) => bytes,
            None => return Ok(None),
        };

        let slot = u64::from_be_bytes(
            slot_bytes
                .try_into()
                .map_err(|_| StorageError::Corruption("invalid slot in account_index".into()))?,
        );

        self.get_account_at_slot(address, slot)
    }

    /// Get an account at a specific slot.
    pub fn get_account_at_slot(
        &self,
        address: &Hash,
        slot: u64,
    ) -> Result<Option<Account>, StorageError> {
        let key = account_key(address, slot);
        match self.get_cf(CF_ACCOUNTS, &key)? {
            Some(bytes) => {
                let account = Account::try_from_slice(&bytes)
                    .map_err(|e| StorageError::Deserialization(e.to_string()))?;
                Ok(Some(account))
            }
            None => Ok(None),
        }
    }

    /// Rewind the account index so each address points to the latest version
    /// at or before `max_slot`. Returns the count of rewound entries.
    /// All updates are collected into a single atomic WriteBatch.
    pub fn rewind_account_index_to_slot(&self, max_slot: u64) -> Result<u64, StorageError> {
        let cf_index = self
            .db
            .cf_handle(CF_ACCOUNT_INDEX)
            .ok_or(StorageError::CfNotFound(CF_ACCOUNT_INDEX))?;

        let mut batch = StorageWriteBatch::new();
        let mut count = 0u64;

        // Iterate all account index entries
        let iter = self.db.iterator_cf(cf_index, IteratorMode::Start);
        for item in iter {
            let (key, value) = item.map_err(StorageError::RocksDb)?;
            if key.len() != 64 || value.len() != 8 {
                continue;
            }

            let current_slot = u64::from_be_bytes(
                value[..8]
                    .try_into()
                    .map_err(|_| StorageError::Corruption("invalid slot bytes".into()))?,
            );

            if current_slot > max_slot {
                let address = Hash::new(
                    key[..64]
                        .try_into()
                        .map_err(|_| StorageError::Corruption("invalid address".into()))?,
                );

                let history = self.get_account_history(&address, 512)?;
                let best = history.iter().find(|(slot, _)| *slot <= max_slot);

                match best {
                    Some((slot, _)) => {
                        batch.put(CF_ACCOUNT_INDEX, address.as_bytes().to_vec(), slot.to_be_bytes().to_vec());
                    }
                    None => {
                        batch.delete(CF_ACCOUNT_INDEX, address.as_bytes().to_vec());
                    }
                }
                count += 1;
            }
        }

        if !batch.is_empty() {
            self.write(&batch)?;
        }
        Ok(count)
    }

    /// Fork-aware rewind: ensure each account index entry points to a version
    /// from the given set of ancestor slots. Accounts whose current version
    /// is NOT in the ancestry are rewound to the latest version that IS.
    ///
    /// Unlike `rewind_account_index_to_slot` which uses a simple `<= max_slot`
    /// comparison, this function correctly handles cross-fork contamination:
    /// if validator V2 replayed blocks on fork A (slots 48-51) and then
    /// switches to replay on fork B (ancestry [52, 47, 43, ...]), the simple
    /// rewind would keep slot 47 data (47 <= 52) even though slot 47 might
    /// be from a different fork. This function checks membership in the
    /// actual ancestry set.
    ///
    /// Accounts pointing to slots at or below the fork tree root (the minimum
    /// slot in the ancestor set) are treated as valid finalized history and
    /// are never rewound. The ancestry from `get_ancestry()` only extends
    /// back to the current fork tree root — after root advancement, earlier
    /// finalized slots (including genesis slot 0) are pruned from the tree.
    /// Without this guard, genesis accounts would be incorrectly deleted.
    pub fn rewind_account_index_for_ancestry(
        &self,
        ancestor_slots: &HashSet<u64>,
    ) -> Result<u64, StorageError> {
        let cf_index = self
            .db
            .cf_handle(CF_ACCOUNT_INDEX)
            .ok_or(StorageError::CfNotFound(CF_ACCOUNT_INDEX))?;

        let root_slot = ancestor_slots.iter().copied().min().unwrap_or(0);

        let mut batch = StorageWriteBatch::new();
        let mut count = 0u64;

        let iter = self.db.iterator_cf(cf_index, IteratorMode::Start);
        for item in iter {
            let (key, value) = item.map_err(StorageError::RocksDb)?;
            if key.len() != 64 || value.len() != 8 {
                continue;
            }

            let current_slot = u64::from_be_bytes(
                value[..8]
                    .try_into()
                    .map_err(|_| StorageError::Corruption("invalid slot bytes".into()))?,
            );

            if current_slot <= root_slot || ancestor_slots.contains(&current_slot) {
                continue;
            }

            let address = Hash::new(
                key[..64]
                    .try_into()
                    .map_err(|_| StorageError::Corruption("invalid address".into()))?,
            );

            let history = self.get_account_history(&address, 512)?;
            let best = history
                .iter()
                .find(|(slot, _)| *slot <= root_slot || ancestor_slots.contains(slot));

            match best {
                Some((slot, _)) => {
                    batch.put(CF_ACCOUNT_INDEX, address.as_bytes().to_vec(), slot.to_be_bytes().to_vec());
                }
                None => {
                    batch.delete(CF_ACCOUNT_INDEX, address.as_bytes().to_vec());
                }
            }
            count += 1;
        }

        if !batch.is_empty() {
            self.write(&batch)?;
        }
        Ok(count)
    }

    /// Remove account data written at `slot` for the given addresses, and
    /// restore the account index to point to each address's latest version at
    /// or before `parent_slot`.
    ///
    /// Called after a failed `replay_block_full` to undo the pollution from
    /// `execute_slot_parallel`, which writes account deltas to `CF_ACCOUNTS`
    /// and updates `CF_ACCOUNT_INDEX` during execution (before verification).
    pub fn cleanup_failed_slot(
        &self,
        slot: u64,
        parent_slot: u64,
        addresses: &[Hash],
    ) -> Result<u64, StorageError> {
        let mut batch = StorageWriteBatch::new();
        let mut count = 0u64;

        for address in addresses {
            batch.delete(CF_ACCOUNTS, account_key(address, slot).to_vec());

            let history = self.get_account_history(address, 512)?;
            let best = history.iter().find(|(s, _)| *s <= parent_slot);

            match best {
                Some((s, _)) => {
                    batch.put(CF_ACCOUNT_INDEX, address.as_bytes().to_vec(), s.to_be_bytes().to_vec());
                }
                None => {
                    batch.delete(CF_ACCOUNT_INDEX, address.as_bytes().to_vec());
                }
            }
            count += 1;
        }

        if !batch.is_empty() {
            self.write(&batch)?;
        }
        Ok(count)
    }

    /// Fork-aware cleanup after a failed replay attempt. Deletes the
    /// contaminated account data at `slot` and restores the index to the
    /// latest version that IS in the given ancestry set.
    /// All updates are written in a single atomic WriteBatch.
    pub fn cleanup_failed_slot_for_ancestry(
        &self,
        slot: u64,
        addresses: &[Hash],
        ancestor_slots: &HashSet<u64>,
    ) -> Result<u64, StorageError> {
        let mut batch = StorageWriteBatch::new();
        let mut count = 0u64;

        for address in addresses {
            batch.delete(CF_ACCOUNTS, account_key(address, slot).to_vec());

            let history = self.get_account_history(address, 512)?;
            let best = history
                .iter()
                .find(|(s, _)| ancestor_slots.contains(s));

            match best {
                Some((s, _)) => {
                    batch.put(CF_ACCOUNT_INDEX, address.as_bytes().to_vec(), s.to_be_bytes().to_vec());
                }
                None => {
                    batch.delete(CF_ACCOUNT_INDEX, address.as_bytes().to_vec());
                }
            }
            count += 1;
        }

        if !batch.is_empty() {
            self.write(&batch)?;
        }
        Ok(count)
    }

    /// Get all accounts in a given partition for rent collection.
    /// Partition is determined by: first byte of address hash % total_partitions.
    pub fn get_accounts_in_partition(
        &self,
        partition: u64,
        total_partitions: u64,
    ) -> Result<Vec<(Hash, Account)>, StorageError> {
        let cf_index = self
            .db
            .cf_handle(CF_ACCOUNT_INDEX)
            .ok_or(StorageError::CfNotFound(CF_ACCOUNT_INDEX))?;

        let mut results = Vec::new();
        let iter = self.db.iterator_cf(cf_index, IteratorMode::Start);

        for item in iter {
            let (key, _value) = item.map_err(StorageError::RocksDb)?;
            if key.len() != 64 {
                continue;
            }

            // Partition by first byte of address
            let first_byte = key[0] as u64;
            if first_byte % total_partitions != partition {
                continue;
            }

            let address = Hash::new(
                key[..64]
                    .try_into()
                    .map_err(|_| StorageError::Corruption("invalid address".into()))?,
            );

            if let Some(account) = self.get_account(&address)? {
                results.push((address, account));
            }
        }

        Ok(results)
    }

    /// Get all accounts in storage (latest version of each).
    ///
    /// Iterates the account index and loads the current state for every address.
    /// Used for initializing the state Merkle tree at startup.
    pub fn get_all_accounts(&self) -> Result<Vec<(Hash, Account)>, StorageError> {
        let cf_index = self
            .db
            .cf_handle(CF_ACCOUNT_INDEX)
            .ok_or(StorageError::CfNotFound(CF_ACCOUNT_INDEX))?;

        let mut results = Vec::new();
        let iter = self.db.iterator_cf(cf_index, IteratorMode::Start);

        for item in iter {
            let (key, _value) = item.map_err(StorageError::RocksDb)?;
            if key.len() != 64 {
                continue;
            }

            let address = Hash::new(
                key[..64]
                    .try_into()
                    .map_err(|_| StorageError::Corruption("invalid address".into()))?,
            );

            if let Some(account) = self.get_account(&address)? {
                results.push((address, account));
            }
        }

        Ok(results)
    }

    /// Get account history (most recent first) with a limit.
    /// Returns `(slot, Account)` pairs ordered by slot descending.
    pub fn get_account_history(
        &self,
        address: &Hash,
        limit: usize,
    ) -> Result<Vec<(u64, Account)>, StorageError> {
        let cf = self
            .db
            .cf_handle(CF_ACCOUNTS)
            .ok_or(StorageError::CfNotFound(CF_ACCOUNTS))?;

        let prefix = address.as_bytes();
        // Start from the end of this address's range (prefix with max slot)
        let end_key = account_key(address, u64::MAX);
        let iter = self.db.iterator_cf(
            cf,
            IteratorMode::From(&end_key, rocksdb::Direction::Reverse),
        );

        let mut results = Vec::new();
        for item in iter {
            let (key, value) = item.map_err(StorageError::RocksDb)?;
            if key.len() < 72 || &key[..64] != prefix {
                break;
            }
            let slot = u64::from_be_bytes(
                key[64..72]
                    .try_into()
                    .map_err(|_| StorageError::Corruption("invalid slot bytes".into()))?,
            );
            let account = Account::try_from_slice(&value)
                .map_err(|e| StorageError::Deserialization(e.to_string()))?;
            results.push((slot, account));
            if results.len() >= limit {
                break;
            }
        }
        Ok(results)
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

    fn test_account(lamports: u64) -> Account {
        let owner = hash(b"system");
        Account::new(lamports, owner)
    }

    #[test]
    fn put_and_get_account() {
        let (storage, _dir) = temp_storage();
        let addr = hash(b"alice");
        let account = test_account(1000);

        storage.put_account(&addr, 1, &account).unwrap();
        let loaded = storage.get_account(&addr).unwrap().unwrap();
        assert_eq!(loaded, account);
    }

    #[test]
    fn get_missing_account() {
        let (storage, _dir) = temp_storage();
        let addr = hash(b"nonexistent");
        assert_eq!(storage.get_account(&addr).unwrap(), None);
    }

    #[test]
    fn get_account_at_slot() {
        let (storage, _dir) = temp_storage();
        let addr = hash(b"bob");
        let acc1 = test_account(100);
        let acc2 = test_account(200);

        storage.put_account(&addr, 1, &acc1).unwrap();
        storage.put_account(&addr, 5, &acc2).unwrap();

        // Latest should be slot 5
        let latest = storage.get_account(&addr).unwrap().unwrap();
        assert_eq!(latest.lamports, 200);

        // Historical at slot 1
        let hist = storage.get_account_at_slot(&addr, 1).unwrap().unwrap();
        assert_eq!(hist.lamports, 100);

        // Missing slot
        assert_eq!(storage.get_account_at_slot(&addr, 3).unwrap(), None);
    }

    #[test]
    fn account_history() {
        let (storage, _dir) = temp_storage();
        let addr = hash(b"carol");

        for slot in [1, 3, 5, 7, 9] {
            storage
                .put_account(&addr, slot, &test_account(slot * 100))
                .unwrap();
        }

        let history = storage.get_account_history(&addr, 3).unwrap();
        assert_eq!(history.len(), 3);
        // Most recent first
        assert_eq!(history[0].0, 9);
        assert_eq!(history[0].1.lamports, 900);
        assert_eq!(history[1].0, 7);
        assert_eq!(history[2].0, 5);
    }

    #[test]
    fn account_history_limit() {
        let (storage, _dir) = temp_storage();
        let addr = hash(b"dave");

        for slot in 1..=10 {
            storage
                .put_account(&addr, slot, &test_account(slot))
                .unwrap();
        }

        let all = storage.get_account_history(&addr, 100).unwrap();
        assert_eq!(all.len(), 10);

        let limited = storage.get_account_history(&addr, 2).unwrap();
        assert_eq!(limited.len(), 2);
    }

    #[test]
    fn write_account_to_batch_returns_result() {
        // Verify that write_account_to_batch returns Result instead of panicking.
        // A valid account should produce Ok(()), confirming the error-returning
        // signature is properly wired through all callers.
        let mut batch = StorageWriteBatch::new();
        let addr = hash(b"result_test");
        let account = test_account(500);

        let result =
            Storage::append_account_write_with_old(&mut batch, &addr, 1, &account, None);
        assert!(result.is_ok(), "valid account serialization should return Ok");

        // Verify the batch actually has entries (the write was performed)
        assert!(!batch.is_empty());
    }

    #[test]
    fn multiple_accounts_isolated() {
        let (storage, _dir) = temp_storage();
        let addr1 = hash(b"user1");
        let addr2 = hash(b"user2");

        storage.put_account(&addr1, 1, &test_account(100)).unwrap();
        storage.put_account(&addr2, 1, &test_account(200)).unwrap();

        let acc1 = storage.get_account(&addr1).unwrap().unwrap();
        let acc2 = storage.get_account(&addr2).unwrap().unwrap();
        assert_eq!(acc1.lamports, 100);
        assert_eq!(acc2.lamports, 200);

        // History should be isolated
        let hist1 = storage.get_account_history(&addr1, 10).unwrap();
        assert_eq!(hist1.len(), 1);
    }
}
