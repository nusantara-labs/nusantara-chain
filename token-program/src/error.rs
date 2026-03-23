use thiserror::Error;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum TokenError {
    #[error("insufficient token balance: need {need}, have {have}")]
    InsufficientBalance { need: u64, have: u64 },

    #[error("owner mismatch")]
    OwnerMismatch,

    #[error("mint mismatch")]
    MintMismatch,

    #[error("authority mismatch")]
    AuthorityMismatch,

    #[error("insufficient delegation: need {need}, have {have}")]
    InsufficientDelegation { need: u64, have: u64 },

    #[error("account already initialized")]
    AlreadyInitialized,

    #[error("account not initialized")]
    NotInitialized,

    #[error("account is frozen")]
    AccountFrozen,

    #[error("no freeze authority on mint")]
    NoFreezeAuthority,

    #[error("supply overflow")]
    SupplyOverflow,

    #[error("invalid instruction data")]
    InvalidInstructionData(String),

    #[error("missing required account")]
    MissingAccount,

    #[error("missing required signer")]
    MissingSigner,

    #[error("close non-empty account")]
    CloseNonEmpty,
}
