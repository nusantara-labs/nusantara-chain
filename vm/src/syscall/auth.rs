//! Authorization check syscalls.
//!
//! WASM programs use these to query whether a given account was marked as a
//! signer or writable in the original transaction. This is essential for
//! programs that need to enforce ownership or authorization rules before
//! modifying state.

use crate::error::VmError;

/// Check if the account at `account_idx` is a signer of the transaction.
pub fn is_signer(account_idx: usize, privileges: &[(bool, bool)]) -> Result<bool, VmError> {
    let &(is_signer, _) = privileges
        .get(account_idx)
        .ok_or(VmError::AccountNotFound(account_idx))?;
    Ok(is_signer)
}

/// Check if the account at `account_idx` is writable in this transaction.
pub fn is_writable(account_idx: usize, privileges: &[(bool, bool)]) -> Result<bool, VmError> {
    let &(_, is_writable) = privileges
        .get(account_idx)
        .ok_or(VmError::AccountNotFound(account_idx))?;
    Ok(is_writable)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_signer_flags() {
        let privileges = vec![(true, true), (false, true), (true, false)];
        assert!(is_signer(0, &privileges).unwrap());
        assert!(!is_signer(1, &privileges).unwrap());
        assert!(is_signer(2, &privileges).unwrap());
    }

    #[test]
    fn check_writable_flags() {
        let privileges = vec![(true, true), (false, true), (true, false)];
        assert!(is_writable(0, &privileges).unwrap());
        assert!(is_writable(1, &privileges).unwrap());
        assert!(!is_writable(2, &privileges).unwrap());
    }

    #[test]
    fn out_of_bounds_signer() {
        let privileges = vec![(true, true)];
        let err = is_signer(5, &privileges).unwrap_err();
        assert!(matches!(err, VmError::AccountNotFound(5)));
    }

    #[test]
    fn out_of_bounds_writable() {
        let privileges = vec![(true, true)];
        let err = is_writable(5, &privileges).unwrap_err();
        assert!(matches!(err, VmError::AccountNotFound(5)));
    }

    #[test]
    fn empty_privileges() {
        let privileges: Vec<(bool, bool)> = vec![];
        assert!(is_signer(0, &privileges).is_err());
        assert!(is_writable(0, &privileges).is_err());
    }
}
