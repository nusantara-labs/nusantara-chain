#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("crypto error: {0}")]
    Crypto(#[from] nusantara_crypto::CryptoError),

    #[error("invalid instruction: {0}")]
    InvalidInstruction(String),

    #[error("invalid message: {0}")]
    InvalidMessage(String),

    #[error("invalid transaction: {0}")]
    InvalidTransaction(String),

    #[error("insufficient funds: need {needed}, have {available}")]
    InsufficientFunds { needed: u64, available: u64 },

    #[error("account not found")]
    AccountNotFound,

    #[error("program error: {0}")]
    ProgramError(String),
}
