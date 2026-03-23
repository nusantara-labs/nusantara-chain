use nusantara_core::CoreError;
use nusantara_crypto::CryptoError;
use nusantara_storage::StorageError;

#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    // Account errors
    #[error("account not found: index {0}")]
    AccountNotFound(usize),

    #[error("account at index {0} is not writable")]
    AccountNotWritable(usize),

    #[error("account at index {0} is not a signer")]
    AccountNotSigner(usize),

    #[error("account already exists")]
    AccountAlreadyExists,

    #[error("account owner mismatch")]
    AccountOwnerMismatch,

    #[error("account data too large: {size} bytes exceeds {limit} byte limit")]
    AccountDataTooLarge { size: u64, limit: u64 },

    #[error("account index aliasing: index {idx_a} and {idx_b} refer to the same account")]
    AccountIndexAliasing { idx_a: usize, idx_b: usize },

    // Balance errors
    #[error("insufficient funds: need {needed}, have {available}")]
    InsufficientFunds { needed: u64, available: u64 },

    #[error("lamports overflow")]
    LamportsOverflow,

    #[error("rent not met: need {needed}, have {available}")]
    RentNotMet { needed: u64, available: u64 },

    // Compute errors
    #[error("insufficient compute units: need {needed}, have {remaining}")]
    InsufficientComputeUnits { needed: u64, remaining: u64 },

    #[error("compute unit limit exceeded")]
    ComputeUnitLimitExceeded,

    // Instruction errors
    #[error("invalid instruction data: {0}")]
    InvalidInstructionData(String),

    #[error("invalid account data: {0}")]
    InvalidAccountData(String),

    #[error("invalid compute budget: {0}")]
    InvalidComputeBudget(String),

    // Program errors
    #[error("unknown program: {0}")]
    UnknownProgram(String),

    #[error("program {program} error: {message}")]
    ProgramError { program: String, message: String },

    // Transaction errors
    #[error("signature verification failed: {0}")]
    SignatureVerificationFailed(String),

    #[error("blockhash not found")]
    BlockhashNotFound,

    #[error("duplicate transaction")]
    DuplicateTransaction,

    // Integrity errors
    #[error("missing required signer at index {0}")]
    MissingRequiredSigner(usize),

    #[error("readonly account modified at index {0}")]
    ReadonlyAccountModified(usize),

    #[error("executable account modified")]
    ExecutableAccountModified,

    // Data size errors
    #[error("loaded accounts data size exceeded: {size} > {limit}")]
    LoadedAccountsDataSizeExceeded { size: u64, limit: u64 },

    // WASM / CPI errors
    #[error("wasm execution error: {0}")]
    WasmError(String),

    #[error("CPI depth exceeded: {depth} > {max}")]
    CpiDepthExceeded { depth: u32, max: u32 },

    #[error("program not executable: {0}")]
    ProgramNotExecutable(String),

    #[error("reentrancy not allowed: {0}")]
    ReentrancyNotAllowed(String),

    #[error("CPI privilege escalation: {0}")]
    CpiPrivilegeEscalation(String),

    // Upstream errors
    #[error("storage error: {0}")]
    Storage(#[from] StorageError),

    #[error("core error: {0}")]
    Core(#[from] CoreError),

    #[error("crypto error: {0}")]
    Crypto(#[from] CryptoError),
}
