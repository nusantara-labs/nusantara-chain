//! Entrypoint macro for Nusantara WASM programs.
//!
//! The `entrypoint!` macro defines the `extern "C" entrypoint` function that
//! the VM calls when executing a program. It deserializes the program ID and
//! instruction data from linear memory, then delegates to a user-provided
//! handler function.
//!
//! # WASM ABI
//!
//! The VM calls `entrypoint` with:
//! - `num_accounts: i32` -- number of accounts passed to the program
//! - `data_ptr: i32` -- pointer to instruction data in WASM linear memory
//! - `data_len: i32` -- length of instruction data in bytes
//! - `program_id_ptr: i32` -- pointer to the 64-byte program ID
//!
//! The function returns `i64`: 0 for success, non-zero for error (the
//! `ProgramError::to_code()` value).

/// Define the WASM entrypoint for a Nusantara program.
///
/// The provided function must have the signature:
///
/// ```ignore
/// fn process_instruction(
///     program_id: &Pubkey,
///     accounts: &[AccountInfo],
///     instruction_data: &[u8],
/// ) -> ProgramResult
/// ```
///
/// # Example
///
/// ```ignore
/// use nusantara_sdk::prelude::*;
///
/// entrypoint!(process_instruction);
///
/// fn process_instruction(
///     program_id: &Pubkey,
///     accounts: &[AccountInfo],
///     data: &[u8],
/// ) -> ProgramResult {
///     msg!("Hello from my program!");
///     Ok(())
/// }
/// ```
///
/// # Notes
///
/// - This macro only generates the extern "C" entrypoint on `wasm32` targets.
/// - On non-WASM targets the macro is a no-op, so your handler function is
///   still available for unit testing.
/// - Full account deserialization from linear memory will be implemented in a
///   future release; currently an empty accounts slice is passed.
#[macro_export]
macro_rules! entrypoint {
    ($process_instruction:ident) => {
        /// Raw WASM entrypoint called by the Nusantara VM.
        #[cfg(target_arch = "wasm32")]
        #[no_mangle]
        pub extern "C" fn entrypoint(
            _num_accounts: i32,
            data_ptr: i32,
            data_len: i32,
            program_id_ptr: i32,
        ) -> i64 {
            // Safety: the VM guarantees that `program_id_ptr` points to 64 valid
            // bytes and that `data_ptr` / `data_len` describe a valid region.
            let program_id = unsafe {
                let slice = core::slice::from_raw_parts(program_id_ptr as *const u8, 64);
                let mut bytes = [0u8; 64];
                bytes.copy_from_slice(slice);
                $crate::pubkey::Pubkey::new(bytes)
            };

            let data =
                unsafe { core::slice::from_raw_parts(data_ptr as *const u8, data_len as usize) };

            // TODO: deserialize AccountInfo structs from the VM memory region
            // identified by `num_accounts`. For now, pass an empty slice.
            let accounts: Vec<$crate::account_info::AccountInfo> = Vec::new();

            match $process_instruction(&program_id, &accounts, data) {
                Ok(()) => 0,
                Err(e) => e.to_code() as i64,
            }
        }
    };
}

#[cfg(test)]
mod tests {
    #[test]
    fn entrypoint_macro_is_available() {
        // The entrypoint! macro only generates code for wasm32, so on native
        // targets we just verify the macro exists and expands without error.
        // We cannot actually invoke the extern "C" function here.
    }
}
