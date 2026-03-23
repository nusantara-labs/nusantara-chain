//! Account metadata passed to programs by the Nusantara VM.
//!
//! Each account referenced by a transaction is presented to the program as an
//! `AccountInfo`. The VM populates these from the validator's account database
//! before invoking the program entrypoint.

use crate::pubkey::Pubkey;

/// Information about a single account available to a program invocation.
///
/// The VM deserializes this from the account region of linear memory before
/// calling the program's entrypoint. Programs read/write account state through
/// the `nusa_get_account_data` / `nusa_set_account_data` syscalls, but the
/// metadata fields here are available for authorization and routing decisions.
#[derive(Clone)]
pub struct AccountInfo<'a> {
    /// Public key (address) of this account.
    pub key: &'a Pubkey,
    /// Whether this account signed the transaction.
    pub is_signer: bool,
    /// Whether this account is writable in this transaction.
    pub is_writable: bool,
    /// Lamport balance of this account.
    pub lamports: u64,
    /// Data stored in this account (read-only view).
    pub data: &'a [u8],
    /// Program that owns this account.
    pub owner: &'a Pubkey,
    /// Whether this account contains executable program code.
    pub executable: bool,
}

impl<'a> AccountInfo<'a> {
    /// Construct an `AccountInfo` with all fields specified.
    pub fn new(
        key: &'a Pubkey,
        is_signer: bool,
        is_writable: bool,
        lamports: u64,
        data: &'a [u8],
        owner: &'a Pubkey,
        executable: bool,
    ) -> Self {
        Self {
            key,
            is_signer,
            is_writable,
            lamports,
            data,
            owner,
            executable,
        }
    }

    /// Length of the account's data in bytes.
    pub fn data_len(&self) -> usize {
        self.data.len()
    }

    /// Returns `true` if the account has no data.
    pub fn data_is_empty(&self) -> bool {
        self.data.is_empty()
    }
}

impl<'a> core::fmt::Debug for AccountInfo<'a> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("AccountInfo")
            .field("key", self.key)
            .field("is_signer", &self.is_signer)
            .field("is_writable", &self.is_writable)
            .field("lamports", &self.lamports)
            .field("data_len", &self.data.len())
            .field("owner", self.owner)
            .field("executable", &self.executable)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_info_basic() {
        let key = Pubkey::new([1u8; 64]);
        let owner = Pubkey::new([2u8; 64]);
        let data = vec![10, 20, 30];
        let info = AccountInfo::new(&key, true, true, 1000, &data, &owner, false);
        assert!(info.is_signer);
        assert!(info.is_writable);
        assert_eq!(info.lamports, 1000);
        assert_eq!(info.data_len(), 3);
        assert!(!info.data_is_empty());
        assert!(!info.executable);
    }

    #[test]
    fn account_info_empty_data() {
        let key = Pubkey::zero();
        let owner = Pubkey::zero();
        let info = AccountInfo::new(&key, false, false, 0, &[], &owner, false);
        assert!(info.data_is_empty());
        assert_eq!(info.data_len(), 0);
    }

    #[test]
    fn account_info_debug_does_not_panic() {
        let key = Pubkey::new([0xAA; 64]);
        let owner = Pubkey::new([0xBB; 64]);
        let data = [1, 2, 3, 4, 5];
        let info = AccountInfo::new(&key, true, false, 500, &data, &owner, true);
        let debug = format!("{info:?}");
        assert!(debug.contains("AccountInfo"));
        assert!(debug.contains("is_signer: true"));
    }

    #[test]
    fn account_info_clone() {
        let key = Pubkey::new([5u8; 64]);
        let owner = Pubkey::new([6u8; 64]);
        let data = vec![99];
        let info = AccountInfo::new(&key, true, true, 42, &data, &owner, false);
        let cloned = info.clone();
        assert_eq!(cloned.key, info.key);
        assert_eq!(cloned.lamports, info.lamports);
        assert_eq!(cloned.data, info.data);
    }
}
