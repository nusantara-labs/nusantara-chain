use nusantara_core::Account;
use nusantara_crypto::Hash;
use rocksdb::IteratorMode;
use tracing::instrument;

use crate::cf::{CF_OWNER_INDEX, CF_PROGRAM_INDEX};
use crate::error::StorageError;
use crate::keys::owner_index_key;
use crate::storage::Storage;
use crate::write_batch::StorageWriteBatch;

/// Maximum number of accounts returned by a single index query when no limit is
/// specified. This prevents unbounded iteration from exhausting memory.
const DEFAULT_QUERY_LIMIT: usize = 1000;

impl Storage {
    /// Prepare owner/program index updates as a `StorageWriteBatch` without
    /// writing. The caller can merge this into a larger batch for atomicity.
    pub fn prepare_index_updates(
        &self,
        address: &Hash,
        old_account: Option<&Account>,
        new_account: &Account,
    ) -> StorageWriteBatch {
        let mut batch = StorageWriteBatch::new();
        Self::write_index_updates(&mut batch, address, old_account, new_account);
        batch
    }

    /// Append owner/program index updates directly into the caller's batch.
    pub(crate) fn write_index_updates(
        batch: &mut StorageWriteBatch,
        address: &Hash,
        old_account: Option<&Account>,
        new_account: &Account,
    ) {
        let new_owner = &new_account.owner;
        let owner_changed = old_account.is_none_or(|old| old.owner != *new_owner);

        if owner_changed {
            if let Some(old) = old_account {
                let old_key = owner_index_key(&old.owner, address);
                batch.delete(CF_OWNER_INDEX, old_key.to_vec());
                batch.delete(CF_PROGRAM_INDEX, old_key.to_vec());
            }
            let new_key = owner_index_key(new_owner, address);
            batch.put(CF_OWNER_INDEX, new_key.to_vec(), Vec::new());
            batch.put(CF_PROGRAM_INDEX, new_key.to_vec(), Vec::new());
        }
    }

    /// Atomically update the owner and program indexes when an account is
    /// written. If the account previously existed with a different owner
    /// (or program), the stale index entries are removed before the new ones
    /// are inserted. The batch is written atomically so the indexes are never
    /// in an inconsistent state.
    #[instrument(skip(self, old_account, new_account), fields(address = %address))]
    pub fn update_account_indexes(
        &self,
        address: &Hash,
        old_account: Option<&Account>,
        new_account: &Account,
    ) -> Result<(), StorageError> {
        let batch = self.prepare_index_updates(address, old_account, new_account);
        if !batch.is_empty() {
            self.write(&batch)?;
            metrics::counter!("nusantara_storage_owner_index_updates").increment(1);
        }
        Ok(())
    }

    /// Return all accounts whose `owner` field matches the given hash,
    /// up to `limit` results.
    #[instrument(skip(self), fields(owner = %owner, limit))]
    pub fn get_accounts_by_owner(
        &self,
        owner: &Hash,
        limit: Option<usize>,
    ) -> Result<Vec<(Hash, Account)>, StorageError> {
        self.query_index_cf(CF_OWNER_INDEX, owner, limit.unwrap_or(DEFAULT_QUERY_LIMIT))
    }

    /// Return all accounts whose program (owner) matches the given hash,
    /// up to `limit` results.
    #[instrument(skip(self), fields(program = %program, limit))]
    pub fn get_accounts_by_program(
        &self,
        program: &Hash,
        limit: Option<usize>,
    ) -> Result<Vec<(Hash, Account)>, StorageError> {
        self.query_index_cf(CF_PROGRAM_INDEX, program, limit.unwrap_or(DEFAULT_QUERY_LIMIT))
    }

    /// Remove both owner and program index entries for an account. Called
    /// during account deletion or cleanup.
    #[instrument(skip(self, account), fields(address = %address))]
    pub fn remove_account_indexes(
        &self,
        address: &Hash,
        account: &Account,
    ) -> Result<(), StorageError> {
        let mut batch = StorageWriteBatch::new();
        let key = owner_index_key(&account.owner, address);
        batch.delete(CF_OWNER_INDEX, key.to_vec());
        batch.delete(CF_PROGRAM_INDEX, key.to_vec());
        self.write(&batch)?;
        metrics::counter!("nusantara_storage_owner_index_removals").increment(1);
        Ok(())
    }

    /// Shared implementation for prefix-scanning an index CF and resolving the
    /// referenced accounts from the main account store.
    fn query_index_cf(
        &self,
        cf_name: &'static str,
        prefix_hash: &Hash,
        limit: usize,
    ) -> Result<Vec<(Hash, Account)>, StorageError> {
        let cf = self
            .db
            .cf_handle(cf_name)
            .ok_or(StorageError::CfNotFound(cf_name))?;

        let prefix = prefix_hash.as_bytes();
        let iter = self.db.iterator_cf(
            cf,
            IteratorMode::From(prefix, rocksdb::Direction::Forward),
        );

        let mut results = Vec::new();
        for item in iter {
            let (key, _value) = item.map_err(StorageError::RocksDb)?;
            // Keys are 128 bytes: prefix(64) ++ address(64).
            if key.len() != 128 || &key[..64] != prefix {
                break;
            }

            let address = Hash::new(
                key[64..128]
                    .try_into()
                    .map_err(|_| StorageError::Corruption("invalid address in index key".into()))?,
            );

            // Resolve the actual account data from the latest account index.
            if let Some(account) = self.get_account(&address)? {
                results.push((address, account));
            }

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

    fn make_account(lamports: u64, owner: Hash) -> Account {
        Account::new(lamports, owner)
    }

    #[test]
    fn index_created_on_put_account() {
        let (storage, _dir) = temp_storage();
        let owner = hash(b"system");
        let addr = hash(b"alice");
        let account = make_account(1000, owner);

        storage.put_account(&addr, 1, &account).unwrap();

        let by_owner = storage.get_accounts_by_owner(&owner, None).unwrap();
        assert_eq!(by_owner.len(), 1);
        assert_eq!(by_owner[0].0, addr);
        assert_eq!(by_owner[0].1.lamports, 1000);
    }

    #[test]
    fn owner_change_removes_old_index() {
        let (storage, _dir) = temp_storage();
        let owner_a = hash(b"program_a");
        let owner_b = hash(b"program_b");
        let addr = hash(b"bob");

        let acc_a = make_account(500, owner_a);
        storage.put_account(&addr, 1, &acc_a).unwrap();

        // Should be under owner_a
        assert_eq!(
            storage.get_accounts_by_owner(&owner_a, None).unwrap().len(),
            1
        );
        assert!(storage.get_accounts_by_owner(&owner_b, None).unwrap().is_empty());

        // Change owner to owner_b
        let acc_b = make_account(500, owner_b);
        storage.put_account(&addr, 2, &acc_b).unwrap();

        // Now should be under owner_b, not owner_a
        assert!(storage.get_accounts_by_owner(&owner_a, None).unwrap().is_empty());
        assert_eq!(
            storage.get_accounts_by_owner(&owner_b, None).unwrap().len(),
            1
        );
    }

    #[test]
    fn multiple_accounts_same_owner() {
        let (storage, _dir) = temp_storage();
        let owner = hash(b"token_program");
        let addrs: Vec<Hash> = (0..5).map(|i| hash(format!("acc_{i}").as_bytes())).collect();

        for (i, addr) in addrs.iter().enumerate() {
            let account = make_account((i as u64 + 1) * 100, owner);
            storage.put_account(addr, 1, &account).unwrap();
        }

        let results = storage.get_accounts_by_owner(&owner, None).unwrap();
        assert_eq!(results.len(), 5);

        // All returned addresses should be in our set
        for (returned_addr, _) in &results {
            assert!(addrs.contains(returned_addr));
        }
    }

    #[test]
    fn pagination_limit_works() {
        let (storage, _dir) = temp_storage();
        let owner = hash(b"owner");

        for i in 0..10 {
            let addr = hash(format!("addr_{i}").as_bytes());
            let account = make_account(100, owner);
            storage.put_account(&addr, 1, &account).unwrap();
        }

        let limited = storage.get_accounts_by_owner(&owner, Some(3)).unwrap();
        assert_eq!(limited.len(), 3);

        let all = storage.get_accounts_by_owner(&owner, None).unwrap();
        assert_eq!(all.len(), 10);
    }

    #[test]
    fn empty_result_for_unknown_owner() {
        let (storage, _dir) = temp_storage();
        let unknown = hash(b"nonexistent_owner");
        let results = storage.get_accounts_by_owner(&unknown, None).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn program_index_mirrors_owner_index() {
        let (storage, _dir) = temp_storage();
        let program = hash(b"my_program");
        let addr = hash(b"data_account");
        let account = make_account(999, program);

        storage.put_account(&addr, 1, &account).unwrap();

        let by_program = storage.get_accounts_by_program(&program, None).unwrap();
        assert_eq!(by_program.len(), 1);
        assert_eq!(by_program[0].0, addr);
        assert_eq!(by_program[0].1.lamports, 999);
    }

    #[test]
    fn remove_account_indexes_clears_both() {
        let (storage, _dir) = temp_storage();
        let owner = hash(b"owner");
        let addr = hash(b"acct");
        let account = make_account(100, owner);

        storage.put_account(&addr, 1, &account).unwrap();
        assert_eq!(
            storage.get_accounts_by_owner(&owner, None).unwrap().len(),
            1
        );

        storage.remove_account_indexes(&addr, &account).unwrap();
        assert!(storage.get_accounts_by_owner(&owner, None).unwrap().is_empty());
        assert!(storage.get_accounts_by_program(&owner, None).unwrap().is_empty());
    }

    #[test]
    fn same_owner_update_does_not_duplicate() {
        let (storage, _dir) = temp_storage();
        let owner = hash(b"system");
        let addr = hash(b"stable");

        let acc1 = make_account(100, owner);
        storage.put_account(&addr, 1, &acc1).unwrap();

        // Update same account, same owner, different lamports
        let acc2 = make_account(200, owner);
        storage.put_account(&addr, 2, &acc2).unwrap();

        let results = storage.get_accounts_by_owner(&owner, None).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].1.lamports, 200);
    }

    #[test]
    fn program_index_limit() {
        let (storage, _dir) = temp_storage();
        let program = hash(b"prog");

        for i in 0..8 {
            let addr = hash(format!("p_acc_{i}").as_bytes());
            storage
                .put_account(&addr, 1, &make_account(100, program))
                .unwrap();
        }

        let limited = storage.get_accounts_by_program(&program, Some(4)).unwrap();
        assert_eq!(limited.len(), 4);
    }
}
