use nusantara_crypto::Hash;

/// Errors that can occur during WASM program compilation, validation, or execution.
///
/// This enum covers the full lifecycle of a WASM program in the Nusantara VM:
/// bytecode validation, compilation, instantiation, execution traps, syscall
/// failures, CPI violations, and resource exhaustion.
#[derive(Debug, thiserror::Error)]
pub enum VmError {
    #[error("wasm compilation error: {0}")]
    Compilation(String),

    #[error("wasm instantiation error: {0}")]
    Instantiation(String),

    #[error("wasm execution trapped: {0}")]
    Trap(String),

    #[error("wasm validation failed: {0}")]
    Validation(String),

    #[error("bytecode too large: {size} > {max}")]
    BytecodeTooLarge { size: usize, max: usize },

    #[error("missing entrypoint export")]
    MissingEntrypoint,

    #[error("invalid entrypoint signature")]
    InvalidEntrypointSignature,

    #[error("missing memory export")]
    MissingMemory,

    #[error("too many memory pages: {pages} > {max}")]
    TooManyMemoryPages { pages: u64, max: u64 },

    #[error("unbounded memory: module must declare an explicit maximum page count")]
    UnboundedMemory,

    #[error("too many functions: {count} > {max}")]
    TooManyFunctions { count: u32, max: u32 },

    #[error("too many tables: {count} > {max}")]
    TooManyTables { count: u32, max: u32 },

    #[error("too many table elements: {count} > {max}")]
    TooManyTableElements { count: u32, max: u32 },

    #[error("too many globals: {count} > {max}")]
    TooManyGlobals { count: u32, max: u32 },

    #[error("too many imports: {count} > {max}")]
    TooManyImports { count: u32, max: u32 },

    #[error("custom section too large: cumulative {bytes} > {max}")]
    CustomSectionTooLarge { bytes: u32, max: u32 },

    #[error("unknown import: module={module:?}, name={name:?} is not in the syscall whitelist")]
    UnknownImport { module: String, name: String },

    #[error("has start function (not allowed)")]
    HasStartFunction,

    #[error("compute units exceeded")]
    ComputeExceeded,

    #[error("memory access out of bounds: offset={offset}, len={len}")]
    MemoryOutOfBounds { offset: u32, len: u32 },

    #[error("CPI depth exceeded: {depth} > {max}")]
    CpiDepthExceeded { depth: u32, max: u32 },

    #[error("reentrancy not allowed: {0}")]
    ReentrancyNotAllowed(String),

    #[error("CPI privilege escalation: {0}")]
    CpiPrivilegeEscalation(String),

    #[error("return data too large: {size} > {max}")]
    ReturnDataTooLarge { size: usize, max: usize },

    #[error("log message too large: {size} > {max}")]
    LogMessageTooLarge { size: usize, max: usize },

    #[error("account not found: index {0}")]
    AccountNotFound(usize),

    #[error("account not writable: index {0}")]
    AccountNotWritable(usize),

    #[error("account owner mismatch at index {account_idx}: expected {expected}, got {got}")]
    AccountOwnerMismatch {
        account_idx: usize,
        expected: Box<Hash>,
        got: Box<Hash>,
    },

    #[error("account not signer: index {0}")]
    AccountNotSigner(usize),

    #[error("program error: code {0}")]
    ProgramError(i64),

    #[error("heap allocation failed: need {need}, have {available}")]
    HeapExhausted { need: u32, available: u32 },

    #[error("syscall error: {0}")]
    Syscall(String),

    #[error("serialization error: {0}")]
    Serialization(String),
}
