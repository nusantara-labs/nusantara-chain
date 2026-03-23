//! Compile-time configuration constants for the Nusantara WASM VM.
//!
//! All values are parsed from `config.toml` at build time via `build.rs`.
//! The build script emits `NUSA_*` environment variables which are read
//! here with `env!()` and converted to typed constants using const-fn parsers.

/// Parse a `u64` from a string at compile time.
/// Assumes the string contains only ASCII digits (no signs, no separators).
const fn const_parse_u64(s: &str) -> u64 {
    let bytes = s.as_bytes();
    let mut result: u64 = 0;
    let mut i = 0;
    while i < bytes.len() {
        result = result * 10 + (bytes[i] - b'0') as u64;
        i += 1;
    }
    result
}

/// Parse a `u32` from a string at compile time.
const fn const_parse_u32(s: &str) -> u32 {
    const_parse_u64(s) as u32
}

// ---------------------------------------------------------------------------
// WASM limits
// ---------------------------------------------------------------------------

/// Maximum bytecode size in bytes (default: 512 KiB).
pub const MAX_WASM_BYTECODE_SIZE: usize =
    const_parse_u64(env!("NUSA_WASM_MAX_BYTECODE_SIZE")) as usize;

/// Maximum initial memory pages a WASM module may declare (default: 64 = 4 MiB).
pub const MAX_MEMORY_PAGES: u32 = const_parse_u32(env!("NUSA_WASM_MAX_MEMORY_PAGES"));

/// Maximum WASM call-stack depth (default: 256).
pub const MAX_CALL_STACK_DEPTH: u32 = const_parse_u32(env!("NUSA_WASM_MAX_CALL_STACK_DEPTH"));

/// Maximum cross-program invocation nesting depth (default: 4).
pub const MAX_CPI_DEPTH: u32 = const_parse_u32(env!("NUSA_WASM_MAX_CPI_DEPTH"));

/// Maximum return-data size in bytes (default: 1 024).
pub const MAX_RETURN_DATA_SIZE: usize =
    const_parse_u64(env!("NUSA_WASM_MAX_RETURN_DATA_SIZE")) as usize;

/// Maximum log message size in bytes (default: 10 000).
pub const MAX_LOG_MESSAGE_SIZE: usize =
    const_parse_u64(env!("NUSA_WASM_MAX_LOG_MESSAGE_SIZE")) as usize;

/// Number of compiled modules the LRU program cache holds (default: 256).
pub const PROGRAM_CACHE_CAPACITY: usize =
    const_parse_u64(env!("NUSA_WASM_PROGRAM_CACHE_CAPACITY")) as usize;

// ---------------------------------------------------------------------------
// Compute-unit costs
// ---------------------------------------------------------------------------

/// Compute units charged for instantiating a WASM module.
pub const COST_INSTANTIATION: u64 = const_parse_u64(env!("NUSA_COST_INSTANTIATION"));

/// Compute units charged per memory page allocated.
pub const COST_MEMORY_PAGE: u64 = const_parse_u64(env!("NUSA_COST_MEMORY_PAGE"));

/// Base compute units charged per syscall invocation.
pub const COST_SYSCALL_BASE: u64 = const_parse_u64(env!("NUSA_COST_SYSCALL_BASE"));

/// Base cost for reading account data.
pub const COST_ACCOUNT_DATA_READ_BASE: u64 =
    const_parse_u64(env!("NUSA_COST_ACCOUNT_DATA_READ_BASE"));

/// Base cost for writing account data.
pub const COST_ACCOUNT_DATA_WRITE_BASE: u64 =
    const_parse_u64(env!("NUSA_COST_ACCOUNT_DATA_WRITE_BASE"));

/// Base cost for a SHA3-512 hash operation.
pub const COST_SHA3_512_BASE: u64 = const_parse_u64(env!("NUSA_COST_SHA3_512_BASE"));

/// Cost for a Dilithium3 signature verification.
pub const COST_SIGNATURE_VERIFY: u64 = const_parse_u64(env!("NUSA_COST_SIGNATURE_VERIFY"));

/// Base cost for a cross-program invocation.
pub const COST_CPI_BASE: u64 = const_parse_u64(env!("NUSA_COST_CPI_BASE"));

/// Base cost for logging a message.
pub const COST_LOG_BASE: u64 = const_parse_u64(env!("NUSA_COST_LOG_BASE"));

/// Cost for a `create_program_address` PDA derivation.
pub const COST_CREATE_PROGRAM_ADDRESS: u64 =
    const_parse_u64(env!("NUSA_COST_CREATE_PROGRAM_ADDRESS"));

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wasm_limits_match_config() {
        assert_eq!(MAX_WASM_BYTECODE_SIZE, 524_288);
        assert_eq!(MAX_MEMORY_PAGES, 64);
        assert_eq!(MAX_CALL_STACK_DEPTH, 256);
        assert_eq!(MAX_CPI_DEPTH, 4);
        assert_eq!(MAX_RETURN_DATA_SIZE, 1024);
        assert_eq!(MAX_LOG_MESSAGE_SIZE, 10_000);
        assert_eq!(PROGRAM_CACHE_CAPACITY, 256);
    }

    #[test]
    fn cost_constants_match_config() {
        assert_eq!(COST_INSTANTIATION, 10_000);
        assert_eq!(COST_MEMORY_PAGE, 1_000);
        assert_eq!(COST_SYSCALL_BASE, 100);
        assert_eq!(COST_ACCOUNT_DATA_READ_BASE, 100);
        assert_eq!(COST_ACCOUNT_DATA_WRITE_BASE, 200);
        assert_eq!(COST_SHA3_512_BASE, 300);
        assert_eq!(COST_SIGNATURE_VERIFY, 2_000);
        assert_eq!(COST_CPI_BASE, 1_000);
        assert_eq!(COST_LOG_BASE, 100);
        assert_eq!(COST_CREATE_PROGRAM_ADDRESS, 1_500);
    }
}
