//! Raw syscall declarations for the Nusantara VM.
//!
//! These are the `extern "C"` host functions that the WASM VM provides to
//! programs. They are only available when compiling to `wasm32-unknown-unknown`.
//! For native testing, stub implementations are provided that return safe
//! default values.
//!
//! # Naming convention
//!
//! All syscalls use the `nusa_` prefix to avoid collisions with other host
//! function namespaces.
//!
//! # Safety
//!
//! All functions are `unsafe` because they read from / write to raw pointers
//! in WASM linear memory. Callers must ensure that pointers and lengths are
//! valid. The higher-level SDK modules wrap these in safe Rust APIs.

// ---------------------------------------------------------------------------
// WASM host function imports
// ---------------------------------------------------------------------------

#[cfg(target_arch = "wasm32")]
unsafe extern "C" {
    // -- Logging --------------------------------------------------------

    /// Write a UTF-8 log message. `ptr` points to the string, `len` is its
    /// byte length. The VM copies the message and appends it to the
    /// transaction log.
    pub safe fn nusa_log(ptr: *const u8, len: i32);

    /// Log the number of compute units remaining. No arguments; the VM reads
    /// the current meter and emits a log line.
    pub safe fn nusa_log_compute_units();

    // -- Account data access --------------------------------------------

    /// Read `len` bytes of account data starting at `offset` for account at
    /// index `account_idx`. Data is written to `dst`. Returns 0 on success,
    /// non-zero on error (out of bounds, invalid index).
    pub safe fn nusa_get_account_data(account_idx: i32, offset: i32, len: i32, dst: *mut u8)
    -> i32;

    /// Write `len` bytes to account data at `offset` for account at index
    /// `account_idx`. Data is read from `src`. Returns 0 on success.
    pub safe fn nusa_set_account_data(
        account_idx: i32,
        offset: i32,
        len: i32,
        src: *const u8,
    ) -> i32;

    /// Get the lamport balance of the account at `account_idx`.
    pub safe fn nusa_get_lamports(account_idx: i32) -> u64;

    /// Set the lamport balance of the account at `account_idx`. Returns 0 on
    /// success.
    pub safe fn nusa_set_lamports(account_idx: i32, lamports: u64) -> i32;

    /// Copy the 64-byte owner pubkey of account `account_idx` into `dst`.
    /// Returns 0 on success.
    pub safe fn nusa_get_owner(account_idx: i32, dst: *mut u8) -> i32;

    // -- Authorization --------------------------------------------------

    /// Returns 1 if the account at `account_idx` is a signer, 0 otherwise.
    pub safe fn nusa_is_signer(account_idx: i32) -> i32;

    /// Returns 1 if the account at `account_idx` is writable, 0 otherwise.
    pub safe fn nusa_is_writable(account_idx: i32) -> i32;

    // -- Cryptography ---------------------------------------------------

    /// Compute the SHA3-512 hash of `data_len` bytes at `data`. The 64-byte
    /// result is written to `result`.
    pub safe fn nusa_sha3_512(data: *const u8, data_len: i32, result: *mut u8);

    /// Verify a Dilithium3 signature. `pubkey` is 1952 bytes, `signature` is
    /// 3309 bytes, `message` is `message_len` bytes. Returns 0 if valid.
    pub safe fn nusa_verify_signature(
        pubkey: *const u8,
        message: *const u8,
        message_len: i32,
        signature: *const u8,
    ) -> i32;

    // -- Sysvar access --------------------------------------------------

    /// Read the Clock sysvar fields into the provided pointers.
    pub safe fn nusa_get_clock(slot: *mut u64, epoch: *mut u64, timestamp: *mut i64);

    /// Read the Rent sysvar fields into the provided pointers.
    pub safe fn nusa_get_rent(
        lamports_per_byte_year: *mut u64,
        exemption_threshold: *mut u64,
        burn_percent: *mut u8,
    );

    /// Read the EpochSchedule sysvar field into the provided pointer.
    pub safe fn nusa_get_epoch_schedule(slots_per_epoch: *mut u64);

    // -- Cross-program invocation ---------------------------------------

    /// Invoke another program. `program_id` is a 64-byte pubkey pointer.
    /// `num_accounts` is the number of account indices to pass. `data` /
    /// `data_len` describe the instruction data. Returns 0 on success.
    pub safe fn nusa_invoke(
        program_id: *const u8,
        num_accounts: i32,
        data: *const u8,
        data_len: i32,
    ) -> i64;

    // -- Return data ----------------------------------------------------

    /// Set the return data for this program invocation.
    pub safe fn nusa_set_return_data(data: *const u8, len: i32);

    /// Read return data from the last CPI call. `program_id` (64 bytes) and
    /// `data` (up to `max_len` bytes) are written. Returns the actual data
    /// length, or -1 if no return data is available.
    pub safe fn nusa_get_return_data(program_id: *mut u8, data: *mut u8, max_len: i32) -> i32;

    // -- Program Derived Address ----------------------------------------

    /// Derive a program address from seeds and a program ID. `seeds_buf`
    /// contains length-prefixed seeds (u32 LE length + bytes per seed).
    /// `seeds_buf_len` is the total byte length of the buffer. `num_seeds`
    /// is the number of seeds. `program_id` is a 64-byte pubkey pointer.
    /// The 64-byte result is written to `result`. Returns 0 on success.
    pub safe fn nusa_create_program_address(
        seeds_buf: *const u8,
        seeds_buf_len: i32,
        num_seeds: i32,
        program_id: *const u8,
        result: *mut u8,
    ) -> i32;

    // -- Memory allocation ----------------------------------------------

    /// Allocate `size` bytes from the program heap. Returns the offset in
    /// linear memory, or -1 on failure.
    pub safe fn nusa_alloc(size: i32) -> i32;
}

// ---------------------------------------------------------------------------
// Native stubs for testing outside WASM
// ---------------------------------------------------------------------------

/// No-op stub for `nusa_log` on non-WASM targets.
///
/// # Safety
///
/// `_ptr` is not dereferenced on native targets; this is a no-op.
#[cfg(not(target_arch = "wasm32"))]
pub unsafe fn nusa_log(_ptr: *const u8, _len: i32) {}

/// No-op stub for `nusa_log_compute_units` on non-WASM targets.
///
/// # Safety
///
/// No pointers are accessed; this is a no-op.
#[cfg(not(target_arch = "wasm32"))]
pub unsafe fn nusa_log_compute_units() {}

/// No-op stub for `nusa_invoke` on non-WASM targets. Always returns 0 (success).
///
/// # Safety
///
/// No pointers are dereferenced on native targets; this is a no-op.
#[cfg(not(target_arch = "wasm32"))]
pub unsafe fn nusa_invoke(
    _program_id: *const u8,
    _num_accounts: i32,
    _data: *const u8,
    _data_len: i32,
) -> i64 {
    0
}

/// Stub for `nusa_get_clock` on non-WASM targets. Writes zeros to all fields.
///
/// # Safety
///
/// Caller must ensure `slot`, `epoch`, and `timestamp` are valid, aligned,
/// writable pointers.
#[cfg(not(target_arch = "wasm32"))]
pub unsafe fn nusa_get_clock(slot: *mut u64, epoch: *mut u64, timestamp: *mut i64) {
    unsafe {
        *slot = 0;
        *epoch = 0;
        *timestamp = 0;
    }
}

/// Stub for `nusa_get_rent` on non-WASM targets. Writes default rent values.
///
/// # Safety
///
/// Caller must ensure all pointers are valid, aligned, and writable.
#[cfg(not(target_arch = "wasm32"))]
pub unsafe fn nusa_get_rent(
    lamports_per_byte_year: *mut u64,
    exemption_threshold: *mut u64,
    burn_percent: *mut u8,
) {
    unsafe {
        *lamports_per_byte_year = 3480;
        *exemption_threshold = 2;
        *burn_percent = 50;
    }
}

/// Stub for `nusa_get_epoch_schedule` on non-WASM targets. Writes zero.
///
/// # Safety
///
/// Caller must ensure `slots_per_epoch` is a valid, aligned, writable pointer.
#[cfg(not(target_arch = "wasm32"))]
pub unsafe fn nusa_get_epoch_schedule(slots_per_epoch: *mut u64) {
    unsafe {
        *slots_per_epoch = 0;
    }
}

/// Stub for `nusa_create_program_address` on non-WASM targets.
///
/// Performs the SHA3-512 PDA derivation natively so that SDK tests produce
/// correct results without a running VM.
///
/// # Safety
///
/// Caller must ensure all pointers are valid and properly sized.
#[cfg(not(target_arch = "wasm32"))]
pub unsafe fn nusa_create_program_address(
    seeds_buf: *const u8,
    seeds_buf_len: i32,
    _num_seeds: i32,
    program_id: *const u8,
    result: *mut u8,
) -> i32 {
    use sha3::{Digest, Sha3_512};
    unsafe {
        let buf = core::slice::from_raw_parts(seeds_buf, seeds_buf_len as usize);
        let pid = core::slice::from_raw_parts(program_id, 64);

        let mut hasher = Sha3_512::new();
        let mut offset = 0;
        while offset < buf.len() {
            if offset + 4 > buf.len() {
                return -1;
            }
            let len = u32::from_le_bytes([
                buf[offset],
                buf[offset + 1],
                buf[offset + 2],
                buf[offset + 3],
            ]) as usize;
            offset += 4;
            if offset + len > buf.len() {
                return -1;
            }
            hasher.update(&buf[offset..offset + len]);
            offset += len;
        }
        hasher.update(pid);
        hasher.update(b"ProgramDerivedAddress");

        let hash = hasher.finalize();
        core::ptr::copy_nonoverlapping(hash.as_ptr(), result, 64);
        0
    }
}
