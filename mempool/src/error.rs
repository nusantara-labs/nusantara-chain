use nusantara_crypto::Hash;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum MempoolError {
    #[error("mempool is full (capacity {capacity}), transaction priority too low")]
    Full { capacity: usize },

    #[error("duplicate transaction")]
    DuplicateTransaction,

    #[error("transaction has expired (blockhash not in valid set)")]
    Expired,

    #[error("account {payer} exceeded per-sender limit ({limit} transactions)")]
    AccountLimitExceeded { payer: Hash, limit: usize },
}
