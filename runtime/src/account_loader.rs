use std::collections::HashMap;

use nusantara_core::Account;
use nusantara_crypto::Hash;
use nusantara_storage::Storage;

use crate::error::RuntimeError;

#[derive(Debug)]
pub struct LoadedAccounts {
    pub accounts: Vec<(Hash, Account)>,
    pub total_data_size: u64,
}

#[tracing::instrument(skip_all, fields(account_count = account_keys.len()))]
pub fn load_accounts(
    storage: &Storage,
    account_keys: &[Hash],
    loaded_accounts_data_size_limit: u32,
    account_cache: Option<&HashMap<Hash, Account>>,
) -> Result<LoadedAccounts, RuntimeError> {
    let mut accounts = Vec::with_capacity(account_keys.len());
    let mut total_data_size = 0u64;

    // Reject duplicate account keys — aliased accounts let a transaction
    // debit one copy while crediting another, creating lamports from nothing.
    {
        let mut seen =
            std::collections::HashSet::with_capacity(account_keys.len());
        for (i, address) in account_keys.iter().enumerate() {
            if !seen.insert(address) {
                let first = account_keys
                    .iter()
                    .position(|a| a == address)
                    .unwrap_or(0);
                return Err(RuntimeError::AccountIndexAliasing {
                    idx_a: first,
                    idx_b: i,
                });
            }
        }
    }

    for address in account_keys {
        // Check in-memory cache first, fall back to RocksDB
        let account = if let Some(cached) = account_cache.and_then(|c| c.get(address)) {
            cached.clone()
        } else {
            storage
                .get_account(address)?
                .unwrap_or_else(|| Account::new(0, Hash::zero()))
        };

        total_data_size += account.data.len() as u64;
        if total_data_size > loaded_accounts_data_size_limit as u64 {
            return Err(RuntimeError::LoadedAccountsDataSizeExceeded {
                size: total_data_size,
                limit: loaded_accounts_data_size_limit as u64,
            });
        }

        accounts.push((*address, account));
    }

    Ok(LoadedAccounts {
        accounts,
        total_data_size,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use nusantara_crypto::hash;
    use tempfile::tempdir;

    fn temp_storage() -> (Storage, tempfile::TempDir) {
        let dir = tempdir().unwrap();
        let storage = Storage::open(dir.path()).unwrap();
        (storage, dir)
    }

    #[test]
    fn load_existing_account() {
        let (storage, _dir) = temp_storage();
        let addr = hash(b"alice");
        let account = Account::new(1000, hash(b"system"));
        storage.put_account(&addr, 0, &account).unwrap();

        let loaded = load_accounts(&storage, &[addr], u32::MAX, None).unwrap();
        assert_eq!(loaded.accounts.len(), 1);
        assert_eq!(loaded.accounts[0].1.lamports, 1000);
    }

    #[test]
    fn load_missing_creates_default() {
        let (storage, _dir) = temp_storage();
        let addr = hash(b"missing");

        let loaded = load_accounts(&storage, &[addr], u32::MAX, None).unwrap();
        assert_eq!(loaded.accounts.len(), 1);
        assert_eq!(loaded.accounts[0].1.lamports, 0);
        assert_eq!(loaded.accounts[0].1.owner, Hash::zero());
    }

    #[test]
    fn data_size_within_limit() {
        let (storage, _dir) = temp_storage();
        let addr = hash(b"account");
        let mut account = Account::new(1000, hash(b"owner"));
        account.data = vec![0u8; 100];
        storage.put_account(&addr, 0, &account).unwrap();

        let loaded = load_accounts(&storage, &[addr], 200, None).unwrap();
        assert_eq!(loaded.total_data_size, 100);
    }

    #[test]
    fn data_size_exceeded() {
        let (storage, _dir) = temp_storage();
        let addr = hash(b"big_account");
        let mut account = Account::new(1000, hash(b"owner"));
        account.data = vec![0u8; 500];
        storage.put_account(&addr, 0, &account).unwrap();

        let err = load_accounts(&storage, &[addr], 100, None).unwrap_err();
        assert!(matches!(
            err,
            RuntimeError::LoadedAccountsDataSizeExceeded { .. }
        ));
    }

    #[test]
    fn duplicate_account_key_rejected() {
        let (storage, _dir) = temp_storage();
        let addr = hash(b"alice");
        let account = Account::new(1000, hash(b"system"));
        storage.put_account(&addr, 0, &account).unwrap();

        let err = load_accounts(&storage, &[addr, addr], u32::MAX, None).unwrap_err();
        assert!(matches!(
            err,
            RuntimeError::AccountIndexAliasing {
                idx_a: 0,
                idx_b: 1
            }
        ));
    }

    #[test]
    fn mixed_existing_and_missing() {
        let (storage, _dir) = temp_storage();
        let addr1 = hash(b"exists");
        let addr2 = hash(b"not_exists");
        let account = Account::new(500, hash(b"system"));
        storage.put_account(&addr1, 0, &account).unwrap();

        let loaded = load_accounts(&storage, &[addr1, addr2], u32::MAX, None).unwrap();
        assert_eq!(loaded.accounts.len(), 2);
        assert_eq!(loaded.accounts[0].1.lamports, 500);
        assert_eq!(loaded.accounts[1].1.lamports, 0);
    }
}
