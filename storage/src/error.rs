#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("rocksdb error: {0}")]
    RocksDb(#[from] rocksdb::Error),
    #[error("borsh serialization error: {0}")]
    Serialization(String),
    #[error("borsh deserialization error: {0}")]
    Deserialization(String),
    #[error("column family not found: {0}")]
    CfNotFound(&'static str),
    #[error("data corruption: {0}")]
    Corruption(String),
    #[error("I/O error: {0}")]
    Io(String),
}

impl From<std::io::Error> for StorageError {
    fn from(e: std::io::Error) -> Self {
        StorageError::Io(e.to_string())
    }
}
