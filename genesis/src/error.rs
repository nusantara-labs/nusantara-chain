use std::io;

use nusantara_storage::StorageError;

#[derive(Debug, thiserror::Error)]
pub enum GenesisError {
    #[error("storage error: {0}")]
    Storage(#[from] StorageError),

    #[error("config parse error: {0}")]
    ConfigParse(String),

    #[error("config not found: {0}")]
    ConfigNotFound(String),

    #[error("config I/O error: {0}")]
    ConfigIo(#[from] io::Error),

    #[error("genesis already initialized: {0}")]
    AlreadyInitialized(String),

    #[error("no validators configured")]
    NoValidators,

    #[error("invalid address: {0}")]
    InvalidAddress(String),

    #[error("total supply overflow")]
    SupplyOverflow,

    #[error("serialization error: {0}")]
    Serialization(String),
}
