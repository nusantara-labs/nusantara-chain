use thiserror::Error;

#[derive(Debug, Error)]
pub enum TurbineError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("serialization error: {0}")]
    Serialization(String),

    #[error("deserialization error: {0}")]
    Deserialization(String),

    #[error("signature verification failed for shred slot={slot} index={index}")]
    ShredSignatureVerification { slot: u64, index: u32 },

    #[error("shred too large: {size} bytes (max {max_size})")]
    ShredTooLarge { size: usize, max_size: usize },

    #[error("block serialization failed: {0}")]
    BlockSerialization(String),

    #[error("erasure coding error: {0}")]
    ErasureCoding(String),

    #[error("erasure recovery failed: insufficient shreds ({have}/{need})")]
    InsufficientShreds { have: usize, need: usize },

    #[error("deshredding failed: {0}")]
    Deshredding(String),

    #[error("slot {slot} already complete")]
    SlotAlreadyComplete { slot: u64 },

    #[error("storage error: {0}")]
    Storage(#[from] nusantara_storage::StorageError),

    #[error("gossip error: {0}")]
    Gossip(#[from] nusantara_gossip::GossipError),

    #[error("channel send error: {0}")]
    ChannelSend(String),

    #[error("compression error: {0}")]
    Compression(String),

    #[error("decompression error: {0}")]
    Decompression(String),

    #[error("merkle proof verification failed for shred slot={slot} index={index}")]
    MerkleProofVerification { slot: u64, index: u32 },

    #[error("missing shred batch header for slot {slot}")]
    MissingBatchHeader { slot: u64 },
}
