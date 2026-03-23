use thiserror::Error;

#[derive(Debug, Error)]
pub enum TpuError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("QUIC connection error: {0}")]
    QuicConnection(String),

    #[error("QUIC stream error: {0}")]
    QuicStream(String),

    #[error("serialization error: {0}")]
    Serialization(String),

    #[error("deserialization error: {0}")]
    Deserialization(String),

    #[error("rate limited: {reason}")]
    RateLimited { reason: String },

    #[error("transaction too large: {size} bytes (max {max_size})")]
    TransactionTooLarge { size: usize, max_size: usize },

    #[error("invalid transaction: {0}")]
    InvalidTransaction(String),

    #[error("TLS error: {0}")]
    Tls(String),

    #[error("no leader available for forwarding")]
    NoLeader,

    #[error("channel send error: {0}")]
    ChannelSend(String),

    #[error("connection cache error: {0}")]
    ConnectionCache(String),

    #[error("compression error: {0}")]
    Compression(String),

    #[error("decompression error: {0}")]
    Decompression(String),

    #[error("invalid batch: {0}")]
    InvalidBatch(String),
}
