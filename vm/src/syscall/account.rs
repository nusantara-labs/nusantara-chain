//! Account data access syscalls.
//!
//! These functions provide the low-level interface for WASM programs to read
//! and write account state. The executor calls these after copying data
//! between WASM linear memory and Rust.
//!
//! ## Privilege enforcement
//!
//! Write operations (`set_account_data`, `set_lamports`) check the
//! `(is_signer, is_writable)` privilege tuple for the target account index.
//! Attempting to mutate a non-writable account returns
//! [`VmError::AccountNotWritable`].

use nusantara_core::Account;
use nusantara_crypto::Hash;

use crate::error::VmError;

/// Read a range of bytes from an account's data field.
///
/// Returns the requested slice as a `Vec<u8>`. Fails if the account index
/// is out of bounds or the `[offset..offset+len)` range exceeds the
/// account's data length.
pub fn get_account_data(
    account_idx: usize,
    offset: usize,
    len: usize,
    accounts: &[(Hash, Account)],
) -> Result<Vec<u8>, VmError> {
    let (_, account) = accounts
        .get(account_idx)
        .ok_or(VmError::AccountNotFound(account_idx))?;

    let end = offset.checked_add(len).ok_or(VmError::MemoryOutOfBounds {
        offset: offset as u32,
        len: len as u32,
    })?;

    if end > account.data.len() {
        return Err(VmError::MemoryOutOfBounds {
            offset: offset as u32,
            len: len as u32,
        });
    }

    Ok(account.data[offset..end].to_vec())
}

/// Write data into an account's data field at the given offset.
///
/// The account must be marked writable in `privileges`, the calling program
/// must own the account, and the write range must fit within the existing
/// data allocation. Only the owning program may modify an account's data.
pub fn set_account_data(
    account_idx: usize,
    offset: usize,
    data: &[u8],
    accounts: &mut [(Hash, Account)],
    privileges: &[(bool, bool)],
    calling_program_id: &Hash,
) -> Result<(), VmError> {
    let &(_, is_writable) = privileges
        .get(account_idx)
        .ok_or(VmError::AccountNotFound(account_idx))?;

    if !is_writable {
        return Err(VmError::AccountNotWritable(account_idx));
    }

    let (_, account) = accounts
        .get_mut(account_idx)
        .ok_or(VmError::AccountNotFound(account_idx))?;

    // Only the owning program may modify account data.
    if account.owner != *calling_program_id {
        return Err(VmError::AccountOwnerMismatch {
            account_idx,
            expected: Box::new(*calling_program_id),
            got: Box::new(account.owner),
        });
    }

    let end = offset
        .checked_add(data.len())
        .ok_or(VmError::MemoryOutOfBounds {
            offset: offset as u32,
            len: data.len() as u32,
        })?;

    if end > account.data.len() {
        return Err(VmError::MemoryOutOfBounds {
            offset: offset as u32,
            len: data.len() as u32,
        });
    }

    account.data[offset..end].copy_from_slice(data);
    Ok(())
}

/// Read an account's lamport balance.
pub fn get_lamports(account_idx: usize, accounts: &[(Hash, Account)]) -> Result<u64, VmError> {
    let (_, account) = accounts
        .get(account_idx)
        .ok_or(VmError::AccountNotFound(account_idx))?;
    Ok(account.lamports)
}

/// Set an account's lamport balance.
///
/// The account must be marked writable in `privileges`. Debits (reducing the
/// balance) require that the calling program owns the account. Credits
/// (increasing or maintaining the balance) are allowed from any program,
/// following the Solana authorization model.
pub fn set_lamports(
    account_idx: usize,
    lamports: u64,
    accounts: &mut [(Hash, Account)],
    privileges: &[(bool, bool)],
    calling_program_id: &Hash,
) -> Result<(), VmError> {
    let &(_, is_writable) = privileges
        .get(account_idx)
        .ok_or(VmError::AccountNotFound(account_idx))?;

    if !is_writable {
        return Err(VmError::AccountNotWritable(account_idx));
    }

    let (_, account) = accounts
        .get_mut(account_idx)
        .ok_or(VmError::AccountNotFound(account_idx))?;

    // Debit (reducing balance) requires owner authorization.
    // Credit (increasing or equal balance) is allowed from any program.
    if lamports < account.lamports && account.owner != *calling_program_id {
        return Err(VmError::AccountOwnerMismatch {
            account_idx,
            expected: Box::new(*calling_program_id),
            got: Box::new(account.owner),
        });
    }

    account.lamports = lamports;
    Ok(())
}

/// Read the owner hash of an account.
pub fn get_owner(account_idx: usize, accounts: &[(Hash, Account)]) -> Result<Hash, VmError> {
    let (_, account) = accounts
        .get(account_idx)
        .ok_or(VmError::AccountNotFound(account_idx))?;
    Ok(account.owner)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nusantara_crypto::hash;

    fn test_accounts() -> Vec<(Hash, Account)> {
        let owner = hash(b"owner");
        let addr1 = hash(b"account1");
        let addr2 = hash(b"account2");
        let mut acc1 = Account::new(1000, owner);
        acc1.data = vec![1, 2, 3, 4, 5];
        let acc2 = Account::new(500, owner);
        vec![(addr1, acc1), (addr2, acc2)]
    }

    #[test]
    fn get_data_slice() {
        let accounts = test_accounts();
        let data = get_account_data(0, 1, 3, &accounts).unwrap();
        assert_eq!(data, vec![2, 3, 4]);
    }

    #[test]
    fn get_data_full() {
        let accounts = test_accounts();
        let data = get_account_data(0, 0, 5, &accounts).unwrap();
        assert_eq!(data, vec![1, 2, 3, 4, 5]);
    }

    #[test]
    fn get_data_out_of_bounds() {
        let accounts = test_accounts();
        let err = get_account_data(0, 3, 5, &accounts).unwrap_err();
        assert!(matches!(err, VmError::MemoryOutOfBounds { .. }));
    }

    #[test]
    fn get_data_account_not_found() {
        let accounts = test_accounts();
        let err = get_account_data(99, 0, 1, &accounts).unwrap_err();
        assert!(matches!(err, VmError::AccountNotFound(99)));
    }

    #[test]
    fn set_data_success() {
        let owner = hash(b"owner");
        let mut accounts = test_accounts();
        let privileges = vec![(true, true), (false, false)];
        set_account_data(0, 1, &[10, 20], &mut accounts, &privileges, &owner).unwrap();
        assert_eq!(accounts[0].1.data, vec![1, 10, 20, 4, 5]);
    }

    #[test]
    fn set_data_not_writable() {
        let owner = hash(b"owner");
        let mut accounts = test_accounts();
        let privileges = vec![(true, false), (false, false)];
        let err =
            set_account_data(0, 0, &[10], &mut accounts, &privileges, &owner).unwrap_err();
        assert!(matches!(err, VmError::AccountNotWritable(0)));
    }

    #[test]
    fn set_data_out_of_bounds() {
        let owner = hash(b"owner");
        let mut accounts = test_accounts();
        let privileges = vec![(true, true), (false, false)];
        let err =
            set_account_data(0, 4, &[10, 20], &mut accounts, &privileges, &owner).unwrap_err();
        assert!(matches!(err, VmError::MemoryOutOfBounds { .. }));
    }

    #[test]
    fn lamports_get() {
        let accounts = test_accounts();
        assert_eq!(get_lamports(0, &accounts).unwrap(), 1000);
        assert_eq!(get_lamports(1, &accounts).unwrap(), 500);
    }

    #[test]
    fn lamports_set() {
        let owner = hash(b"owner");
        let mut accounts = test_accounts();
        let privileges = vec![(true, true), (false, false)];
        // Credit (increasing balance) is allowed from any program, but use owner here.
        set_lamports(0, 2000, &mut accounts, &privileges, &owner).unwrap();
        assert_eq!(get_lamports(0, &accounts).unwrap(), 2000);
    }

    #[test]
    fn lamports_set_not_writable() {
        let owner = hash(b"owner");
        let mut accounts = test_accounts();
        let privileges = vec![(true, false), (false, false)];
        let err = set_lamports(0, 2000, &mut accounts, &privileges, &owner).unwrap_err();
        assert!(matches!(err, VmError::AccountNotWritable(0)));
    }

    #[test]
    fn owner_get() {
        let accounts = test_accounts();
        let owner = get_owner(0, &accounts).unwrap();
        assert_eq!(owner, nusantara_crypto::hash(b"owner"));
    }

    #[test]
    fn account_not_found() {
        let accounts = test_accounts();
        assert!(matches!(
            get_lamports(99, &accounts).unwrap_err(),
            VmError::AccountNotFound(99)
        ));
        assert!(matches!(
            get_owner(99, &accounts).unwrap_err(),
            VmError::AccountNotFound(99)
        ));
    }

    #[test]
    fn set_lamports_debit_wrong_owner() {
        let attacker = hash(b"attacker_program");
        let mut accounts = test_accounts(); // accounts owned by hash(b"owner")
        let privileges = vec![(true, true), (true, true)];
        // Account 0 has 1000 lamports. Attempting to debit to 500 from a
        // non-owning program must fail.
        let err = set_lamports(0, 500, &mut accounts, &privileges, &attacker).unwrap_err();
        assert!(matches!(err, VmError::AccountOwnerMismatch { .. }));
        // Balance must remain unchanged.
        assert_eq!(accounts[0].1.lamports, 1000);
    }

    #[test]
    fn set_lamports_credit_any_program() {
        let non_owner = hash(b"other_program");
        let mut accounts = test_accounts(); // accounts owned by hash(b"owner")
        let privileges = vec![(true, true), (true, true)];
        // Credit (increasing balance) should succeed even from a non-owning program.
        set_lamports(0, 2000, &mut accounts, &privileges, &non_owner).unwrap();
        assert_eq!(accounts[0].1.lamports, 2000);
    }

    #[test]
    fn set_data_wrong_owner() {
        let attacker = hash(b"attacker_program");
        let mut accounts = test_accounts(); // accounts owned by hash(b"owner")
        let privileges = vec![(true, true), (true, true)];
        // Attempting to write data from a non-owning program must fail.
        let err =
            set_account_data(0, 0, &[99], &mut accounts, &privileges, &attacker).unwrap_err();
        assert!(matches!(err, VmError::AccountOwnerMismatch { .. }));
        // Data must remain unchanged.
        assert_eq!(accounts[0].1.data, vec![1, 2, 3, 4, 5]);
    }

    #[test]
    fn set_data_correct_owner() {
        let owner = hash(b"owner");
        let mut accounts = test_accounts();
        let privileges = vec![(true, true), (true, true)];
        // Writing data from the owning program should succeed.
        set_account_data(0, 0, &[99], &mut accounts, &privileges, &owner).unwrap();
        assert_eq!(accounts[0].1.data, vec![99, 2, 3, 4, 5]);
    }
}
