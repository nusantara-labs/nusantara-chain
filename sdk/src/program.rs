//! Cross-program invocation (CPI) helpers.
//!
//! Programs call other programs via `invoke` or `invoke_signed`. Under WASM
//! these delegate to the `nusa_invoke` syscall; outside WASM they are no-op
//! stubs for unit testing.

use crate::account_info::AccountInfo;
#[cfg(target_arch = "wasm32")]
use crate::program_error::ProgramError;
use crate::program_error::ProgramResult;
use crate::pubkey::Pubkey;

/// Invoke a cross-program instruction.
///
/// Serializes the target program ID, accounts, and instruction data, then
/// calls the `nusa_invoke` host function. The VM validates privileges (signer /
/// writable) and executes the target program synchronously before returning
/// control to the caller.
pub fn invoke(program_id: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    #[cfg(target_arch = "wasm32")]
    {
        let result = unsafe {
            crate::syscall::nusa_invoke(
                program_id.as_bytes().as_ptr(),
                accounts.len() as i32,
                data.as_ptr(),
                data.len() as i32,
            )
        };
        if result == 0 {
            Ok(())
        } else {
            Err(ProgramError::Custom(result as u32))
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        // Stub for testing outside WASM -- does nothing.
        let _ = (program_id, accounts, data);
        Ok(())
    }
}

/// Invoke a cross-program instruction with program-derived address (PDA) signing.
///
/// `signer_seeds` contains the seed slices used to derive one or more PDAs.
/// The VM uses these to verify that the calling program is authorized to sign
/// on behalf of those addresses. Full PDA support will be implemented in a
/// future release; for now this delegates to [`invoke`].
pub fn invoke_signed(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
    _signer_seeds: &[&[&[u8]]],
) -> ProgramResult {
    invoke(program_id, accounts, data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invoke_stub_succeeds() {
        let program_id = Pubkey::zero();
        let result = invoke(&program_id, &[], &[]);
        assert!(result.is_ok());
    }

    #[test]
    fn invoke_signed_stub_succeeds() {
        let program_id = Pubkey::zero();
        let seeds: &[&[&[u8]]] = &[&[b"seed1", b"seed2"]];
        let result = invoke_signed(&program_id, &[], &[], seeds);
        assert!(result.is_ok());
    }

    #[test]
    fn invoke_with_data() {
        let program_id = Pubkey::new([1u8; 64]);
        let data = vec![0, 1, 2, 3, 4, 5, 6, 7, 8, 9];
        let result = invoke(&program_id, &[], &data);
        assert!(result.is_ok());
    }
}
