//! Return-data syscalls: `nusa_set_return_data` and `nusa_get_return_data`.
//!
//! Return data is a small buffer (up to [`MAX_RETURN_DATA_SIZE`] bytes) that
//! a program can set during execution. The calling program (via CPI) or the
//! runtime can read this data after the invocation completes.

use nusantara_crypto::Hash;

use crate::config::MAX_RETURN_DATA_SIZE;
use crate::error::VmError;

/// Set return data for the current invocation.
///
/// Returns [`VmError::ReturnDataTooLarge`] if `data` exceeds the size limit.
pub fn set_return_data(
    program_id: Hash,
    data: Vec<u8>,
    return_data: &mut Option<(Hash, Vec<u8>)>,
) -> Result<(), VmError> {
    if data.len() > MAX_RETURN_DATA_SIZE {
        return Err(VmError::ReturnDataTooLarge {
            size: data.len(),
            max: MAX_RETURN_DATA_SIZE,
        });
    }
    *return_data = Some((program_id, data));
    Ok(())
}

/// Get return data from the most recent invocation (if any).
pub fn get_return_data(return_data: &Option<(Hash, Vec<u8>)>) -> Option<(Hash, Vec<u8>)> {
    return_data.clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use nusantara_crypto::hash;

    #[test]
    fn set_and_get() {
        let mut rd = None;
        let pid = hash(b"program");
        set_return_data(pid, vec![1, 2, 3], &mut rd).unwrap();
        let (id, data) = get_return_data(&rd).unwrap();
        assert_eq!(id, pid);
        assert_eq!(data, vec![1, 2, 3]);
    }

    #[test]
    fn set_empty_data() {
        let mut rd = None;
        let pid = hash(b"program");
        set_return_data(pid, vec![], &mut rd).unwrap();
        let (id, data) = get_return_data(&rd).unwrap();
        assert_eq!(id, pid);
        assert!(data.is_empty());
    }

    #[test]
    fn set_at_limit() {
        let mut rd = None;
        let pid = hash(b"program");
        let data = vec![42u8; MAX_RETURN_DATA_SIZE];
        set_return_data(pid, data.clone(), &mut rd).unwrap();
        let (_, retrieved) = get_return_data(&rd).unwrap();
        assert_eq!(retrieved.len(), MAX_RETURN_DATA_SIZE);
    }

    #[test]
    fn too_large() {
        let mut rd = None;
        let pid = hash(b"program");
        let data = vec![0u8; MAX_RETURN_DATA_SIZE + 1];
        let err = set_return_data(pid, data, &mut rd).unwrap_err();
        assert!(matches!(err, VmError::ReturnDataTooLarge { .. }));
    }

    #[test]
    fn get_empty_returns_none() {
        let rd: Option<(Hash, Vec<u8>)> = None;
        assert!(get_return_data(&rd).is_none());
    }

    #[test]
    fn overwrite_return_data() {
        let mut rd = None;
        let pid1 = hash(b"program1");
        let pid2 = hash(b"program2");
        set_return_data(pid1, vec![1], &mut rd).unwrap();
        set_return_data(pid2, vec![2, 3], &mut rd).unwrap();
        let (id, data) = get_return_data(&rd).unwrap();
        assert_eq!(id, pid2);
        assert_eq!(data, vec![2, 3]);
    }
}
